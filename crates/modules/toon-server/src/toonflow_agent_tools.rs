use crate::toonflow_agent_tool_utils::{
    changed_flow_data, missing_appearance_derivatives, now_ms, required_tagged,
    role_names_without_appearances, script_format_instruction, tagged,
    workspace_format_instruction,
};
use crate::{
    ToonState, ai_client,
    shared::require,
    toonflow_agent_episode_scope::*,
    toonflow_agent_read_tools, toonflow_agent_runtime, toonflow_agent_tool_record, toonflow_agents,
    toonflow_asset_ai,
    toonflow_episode_renders::{ensure_project_access, ensure_script_in_project},
    toonflow_image_workflow,
    toonflow_scene_transitions::{
        apply_track_transition_defaults, normalize_persisted_scene_key,
        persist_work_data_with_transition_sync,
    },
    toonflow_ws::WsEmitter,
};
use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::Deserialize;
use serde_json::{Value, json};
const STORYBOARD_TABLE_AGENT_TOOLS: &[&str] = &["get_flowData", "set_flowData"];

#[cfg(test)]
mod memory_tool_tests {
    use super::{
        DEFER_STORYBOARD_TRANSITION_SYNC, STORYBOARD_TABLE_AGENT_TOOLS,
        StoryboardPanelBatchContext, record_storyboard_panel_created,
        record_storyboard_panel_touched, required_tagged, storyboard_transition_sync_is_deferred,
    };
    use std::sync::Arc;

    #[test]
    fn storyboard_table_agent_can_read_and_persist_its_document() {
        assert!(STORYBOARD_TABLE_AGENT_TOOLS.contains(&"get_flowData"));
        assert!(STORYBOARD_TABLE_AGENT_TOOLS.contains(&"set_flowData"));
    }

    #[test]
    fn missing_workspace_xml_is_returned_as_an_actionable_error() {
        let error = required_tagged("没有标签", "storySkeleton").unwrap_err();
        assert!(error.to_string().contains("未输出 <storySkeleton> 标签"));
    }

    #[tokio::test]
    async fn storyboard_transition_sync_is_deferred_only_inside_panel_batch() {
        assert!(!storyboard_transition_sync_is_deferred());
        let context = Arc::new(StoryboardPanelBatchContext::default());
        DEFER_STORYBOARD_TRANSITION_SYNC
            .scope(context.clone(), async {
                assert!(storyboard_transition_sync_is_deferred());
                record_storyboard_panel_touched(7);
                record_storyboard_panel_created(8);
                record_storyboard_panel_created(8);
            })
            .await;
        assert!(!storyboard_transition_sync_is_deferred());
        assert_eq!(context.touched_ids(), vec![7, 8]);
        assert_eq!(context.created_ids(), vec![8]);
    }
}

pub use crate::toonflow_agent_plan::{get_plan, set_plan, update_plan};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ToolRequest {
    pub(crate) agent_type: String,
    /// Concrete runtime agent identity. Internal-only so API callers cannot
    /// widen their Skill scope by supplying a different stage agent.
    #[serde(skip, default)]
    pub(crate) agent_key: Option<String>,
    #[serde(default)]
    pub(crate) isolation_key: String,
    pub(crate) project_id: i64,
    pub(crate) script_id: Option<i64>,
    pub(crate) tool_name: String,
    #[serde(default)]
    pub(crate) arguments: Value,
    #[serde(skip, default)]
    pub(crate) emitter: Option<WsEmitter>,
}

#[derive(sqlx::FromRow)]
struct StoryboardAgentEditRow {
    prompt: String,
    should_generate_image: i32,
    script_id: i64,
    track_id: Option<i64>,
    track: Option<String>,
    scene_key: Option<String>,
    scene_state_id: Option<i64>,
}

#[derive(Default)]
struct StoryboardPanelBatchContext {
    created_ids: std::sync::Mutex<Vec<i64>>,
    touched_ids: std::sync::Mutex<Vec<i64>>,
}

impl StoryboardPanelBatchContext {
    fn push_unique(ids: &std::sync::Mutex<Vec<i64>>, id: i64) {
        let mut ids = ids.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    fn record_created(&self, id: i64) {
        Self::push_unique(&self.created_ids, id);
        Self::push_unique(&self.touched_ids, id);
    }

    fn record_touched(&self, id: i64) {
        Self::push_unique(&self.touched_ids, id);
    }

    fn created_ids(&self) -> Vec<i64> {
        self.created_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn touched_ids(&self) -> Vec<i64> {
        self.touched_ids
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

tokio::task_local! {
    /// The storyboard-panel sub-agent writes one panel as a sequence of tool calls.
    /// Recomputing every track transition after each row turns that batch into O(N²).
    static DEFER_STORYBOARD_TRANSITION_SYNC: std::sync::Arc<StoryboardPanelBatchContext>;
}

fn storyboard_transition_sync_is_deferred() -> bool {
    DEFER_STORYBOARD_TRANSITION_SYNC.try_with(|_| ()).is_ok()
}

fn record_storyboard_panel_created(id: i64) {
    let _ = DEFER_STORYBOARD_TRANSITION_SYNC.try_with(|context| context.record_created(id));
}

fn record_storyboard_panel_touched(id: i64) {
    let _ = DEFER_STORYBOARD_TRANSITION_SYNC.try_with(|context| context.record_touched(id));
}

async fn apply_storyboard_transition_defaults_if_ready(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
) -> Result<(), AppError> {
    if storyboard_transition_sync_is_deferred() {
        return Ok(());
    }
    crate::toonflow_scene_transitions::apply_track_transition_defaults(pool, project_id, script_id)
        .await
}

struct StoryboardPanelBatchGuard {
    pool: sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    context: std::sync::Arc<StoryboardPanelBatchContext>,
    armed: bool,
}

impl StoryboardPanelBatchGuard {
    fn new(
        pool: sqlx::PgPool,
        project_id: i64,
        script_id: i64,
        context: std::sync::Arc<StoryboardPanelBatchContext>,
    ) -> Self {
        Self {
            pool,
            project_id,
            script_id,
            context,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StoryboardPanelBatchGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let pool = self.pool.clone();
        let project_id = self.project_id;
        let script_id = self.script_id;
        let created_ids = self.context.created_ids();
        runtime.spawn(async move {
            if let Err(error) =
                rollback_storyboard_panel_batch(&pool, project_id, script_id, &created_ids).await
            {
                tracing::warn!(
                    project_id,
                    script_id,
                    error = ?error,
                    "failed to finalize canceled storyboard panel batch"
                );
            }
        });
    }
}

async fn rollback_storyboard_panel_batch(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    created_ids: &[i64],
) -> Result<(), AppError> {
    let mut created_ids = created_ids.to_vec();
    created_ids.sort_unstable();
    created_ids.dedup();
    if created_ids.is_empty() {
        return apply_track_transition_defaults(pool, project_id, script_id).await;
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin storyboard rollback"))?;
    let mut track_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT track_id FROM toonflow.storyboards
         WHERE id=ANY($1) AND project_id=$2 AND script_id=$3 AND track_id IS NOT NULL
         FOR UPDATE",
    )
    .bind(&created_ids)
    .bind(project_id)
    .bind(script_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to find partial storyboard tracks"))?;
    track_ids.sort_unstable();
    track_ids.dedup();
    sqlx::query(
        "DELETE FROM toonflow.assets_storyboards
         WHERE storyboard_id IN (
           SELECT id FROM toonflow.storyboards
           WHERE id=ANY($1) AND project_id=$2 AND script_id=$3
         )",
    )
    .bind(&created_ids)
    .bind(project_id)
    .bind(script_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to remove partial storyboard assets"))?;
    sqlx::query(
        "DELETE FROM toonflow.storyboards
         WHERE id=ANY($1) AND project_id=$2 AND script_id=$3",
    )
    .bind(&created_ids)
    .bind(project_id)
    .bind(script_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to remove partial storyboard rows"))?;
    sqlx::query(
        "UPDATE toonflow.video_tracks track
         SET duration=(
           SELECT coalesce(sum(CASE WHEN board.duration ~ '^[0-9]+$'
             THEN board.duration::integer ELSE 0 END),0)::integer
           FROM toonflow.storyboards board WHERE board.track_id=track.id
         )
         WHERE track.id=ANY($1) AND track.project_id=$2 AND track.script_id=$3",
    )
    .bind(&track_ids)
    .bind(project_id)
    .bind(script_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to repair partial storyboard track duration"))?;
    sqlx::query(
        "DELETE FROM toonflow.video_tracks track
         WHERE track.id=ANY($1) AND track.project_id=$2 AND track.script_id=$3
           AND NOT EXISTS (
             SELECT 1 FROM toonflow.storyboards board WHERE board.track_id=track.id
           )",
    )
    .bind(&track_ids)
    .bind(project_id)
    .bind(script_id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to remove partial storyboard tracks"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to commit storyboard rollback"))?;
    apply_track_transition_defaults(pool, project_id, script_id).await
}

pub async fn execute(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ToolRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    ensure_project_access(&state.pool, &user, request.project_id).await?;
    let (call_id, value) = execute_recorded(&state, &request).await?;
    Ok(Json(ApiResponse::new(
        json!({"callId":call_id,"result":value}),
    )))
}

pub(crate) async fn execute_recorded(
    state: &ToonState,
    request: &ToolRequest,
) -> Result<(i64, Value), AppError> {
    if let Some(script_id) = request.script_id {
        ensure_script_in_project(&state.pool, request.project_id, script_id).await?;
    } else {
        let project_exists: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM toonflow.projects WHERE id=$1)")
                .bind(request.project_id)
                .fetch_one(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to validate agent project scope"))?;
        if !project_exists {
            return Err(AppError::not_found("project not found"));
        }
    }
    let call_id = now_ms() * 1000;
    toonflow_agent_tool_record::start(
        &state.pool,
        call_id,
        &request.agent_type,
        &request.tool_name,
        &request.arguments,
        now_ms(),
    )
    .await?;
    match execute_inner(state, request).await {
        Ok(value) => {
            toonflow_agent_tool_record::succeed(&state.pool, call_id, &value, now_ms()).await;
            if request.emitter.is_none()
                && request.agent_type == "productionAgent"
                && let Some(script_id) = request.script_id
            {
                toonflow_agent_tool_record::remember_ui_execution(
                    &state.pool,
                    call_id,
                    request.project_id,
                    script_id,
                    &request.tool_name,
                    &request.arguments,
                    &value,
                    now_ms(),
                )
                .await;
            }
            Ok((call_id, value))
        }
        Err(error) => {
            toonflow_agent_tool_record::fail(&state.pool, call_id, &error, now_ms()).await;
            Err(error)
        }
    }
}

pub(crate) async fn execute_inner(
    state: &ToonState,
    request: &ToolRequest,
) -> Result<Value, AppError> {
    if request.tool_name == "use_skill" || request.tool_name == "read_skill_file" {
        let path = request
            .arguments
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| AppError::bad_request("缺少 Skill path"))?;
        if path.starts_with('/') || path.contains("..") || path.contains('\\') {
            return Err(AppError::bad_request("Skill 路径无效"));
        }
        let default_agent_key = match request.agent_type.as_str() {
            "scriptAgent" => "scriptAgent:decisionAgent",
            "productionAgent" => "productionAgent:decisionAgent",
            _ => return Err(AppError::bad_request("不支持的 Agent 类型")),
        };
        let skill_agent_key = request.agent_key.as_deref().unwrap_or(default_agent_key);
        let available = toonflow_agent_runtime::available_skills(
            &state.pool,
            skill_agent_key,
            request.project_id,
        )
        .await
        .map_err(AppError::bad_request)?;
        if !available
            .iter()
            .any(|(available_path, _, _)| available_path == path)
        {
            return Err(AppError::bad_request(
                "该 Skill 不属于当前 Agent 或项目上下文",
            ));
        }
        let content = toonflow_agent_runtime::load_skill(&state.pool, path)
            .await
            .map_err(AppError::bad_request)?;
        return Ok(json!({
            "path":path, "content":content,
            "sha256":crate::toonflow_prompt_trace::sha256(&content),
            "agentKey":skill_agent_key, "projectId":request.project_id,
        }));
    }
    match (request.agent_type.as_str(), request.tool_name.as_str()) {
        ("scriptAgent", "get_planData") => {
            let key = request
                .arguments
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("scriptAgent");
            if key != "scriptAgent" {
                return Err(AppError::bad_request(
                    "get_planData 仅支持 scriptAgent 工作区",
                ));
            }
            crate::toonflow_agent_plan::load_plan_data(&state.pool, request.project_id).await
        }
        ("scriptAgent" | "productionAgent", "deepRetrieve") => {
            let keyword = request
                .arguments
                .get("keyword")
                .and_then(Value::as_str)
                .unwrap_or("");
            let isolation_key = if request.isolation_key.trim().is_empty() {
                toonflow_agent_tool_record::memory_isolation_key(
                    &request.agent_type,
                    request.project_id,
                    request.script_id,
                )
            } else {
                request.isolation_key.clone()
            };
            let judge_key = if request.agent_type == "scriptAgent" {
                "scriptAgent:decisionAgent"
            } else {
                "productionAgent:decisionAgent"
            };
            let limit =
                toonflow_agent_runtime::setting_usize(&state.pool, "deepRetrieveSummaryLimit", 5)
                    .await;
            let mems = toonflow_agent_runtime::deep_retrieve(
                &state.pool,
                &request.agent_type,
                &isolation_key,
                keyword,
                judge_key,
                limit,
            )
            .await
            .map_err(|_| AppError::internal("deepRetrieve 失败"))?;
            Ok(json!(mems.join("\n\n")))
        }
        ("scriptAgent", "get_novel_events") => {
            let indexes = request
                .arguments
                .get("chapterIndexs")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_i64())
                .collect::<Vec<_>>();
            let rows:Vec<(i32,String,Option<String>)>=sqlx::query_as("SELECT chapter_index,chapter,event FROM toonflow.novels WHERE project_id=$1 AND chapter_index=ANY($2) ORDER BY chapter_index").bind(request.project_id).bind(indexes.iter().map(|v|*v as i32).collect::<Vec<_>>()).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to get novel events"))?;
            Ok(json!(
                rows.into_iter()
                    .map(|(i, c, e)| format!(
                        "第{i}章「{c}」\n{}",
                        toonflow_agent_read_tools::format_events(&e.unwrap_or_default())
                    ))
                    .collect::<Vec<_>>()
                    .join("\n\n")
            ))
        }
        ("scriptAgent", "get_novel_text") => {
            let index = request
                .arguments
                .get("chapterIndex")
                .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                .ok_or_else(|| AppError::bad_request("缺少 chapterIndex"))?;
            let text: Option<String> = sqlx::query_scalar(
                "SELECT chapter_data FROM toonflow.novels WHERE project_id=$1 AND chapter_index=$2",
            )
            .bind(request.project_id)
            .bind(index as i32)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get novel text"))?;
            Ok(json!(text.unwrap_or_default()))
        }
        ("scriptAgent", "get_script_content") => {
            let ids = request
                .arguments
                .get("ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
                .collect::<Vec<_>>();
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT name,content FROM toonflow.scripts WHERE project_id=$1 AND id=ANY($2)",
            )
            .bind(request.project_id)
            .bind(ids)
            .fetch_all(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get scripts"))?;
            Ok(json!(
                rows.into_iter()
                    .map(|(n, c)| format!("<scriptItem name=\"{n}\">{c}</scriptItem>"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))
        }
        ("scriptAgent", "save_scripts") => {
            let scripts = request
                .arguments
                .get("scripts")
                .and_then(Value::as_array)
                .ok_or_else(|| AppError::bad_request("缺少 scripts"))?;
            if scripts.is_empty() {
                return Err(AppError::bad_request("剧本列表不能为空"));
            }
            let normalized = normalize_script_items(scripts)?;
            let mut tx = state
                .pool
                .begin()
                .await
                .map_err(|_| AppError::internal("failed to begin script save"))?;
            let timestamp = now_ms();
            let mut saved = Vec::with_capacity(normalized.len());
            let mut existing = sqlx::query_as::<_, (i64, String)>(
                "SELECT id,name FROM toonflow.scripts WHERE project_id=$1 ORDER BY create_time DESC,id DESC",
            )
            .bind(request.project_id)
            .fetch_all(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to load existing scripts"))?;
            for (index, (name, content)) in normalized.iter().enumerate() {
                if let Err(reason) = validate_script_for_episode(name, content) {
                    return Err(AppError::bad_request(format!(
                        "剧本《{name}》未完整生成：{reason}。请重新调用剧本子 Agent 生成完整正文后再保存"
                    )));
                }
                let episode = script_episode_number(name);
                let matching = existing.iter().position(|(_, existing_name)| {
                    existing_name == name
                        || (episode.is_some() && script_episode_number(existing_name) == episode)
                });
                let id = if let Some(position) = matching {
                    let (id, _) = existing.remove(position);
                    sqlx::query("UPDATE toonflow.scripts SET name=$2,content=$3,extract_state=NULL,error_reason=NULL WHERE id=$1 AND project_id=$4")
                        .bind(id)
                        .bind(name)
                        .bind(content)
                        .bind(request.project_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|_| AppError::internal("failed to replace generated script"))?;
                    id
                } else {
                    let id = timestamp * 1000 + index as i64;
                    let id: i64 = sqlx::query_scalar(
                        "INSERT INTO toonflow.scripts(id,name,content,project_id,create_time)
                         VALUES($1,$2,$3,$4,$5)
                         ON CONFLICT (project_id,name) DO UPDATE
                         SET content=EXCLUDED.content,extract_state=NULL,error_reason=NULL
                         RETURNING id",
                    )
                    .bind(id)
                    .bind(name)
                    .bind(content)
                    .bind(request.project_id)
                    .bind(timestamp)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|_| AppError::internal("failed to save generated script"))?;
                    id
                };
                saved.push(json!({"id":id,"name":name}));
            }
            tx.commit()
                .await
                .map_err(|_| AppError::internal("failed to commit generated scripts"))?;
            Ok(json!({"saved":saved}))
        }
        ("scriptAgent", name)
            if name.starts_with("run_sub_agent_") || name == "run_supervision_agent" =>
        {
            let (agent_key, label, tag) = match name {
                "run_sub_agent_storySkeleton" => (
                    "scriptAgent:storySkeletonAgent",
                    "故事骨架",
                    Some("storySkeleton"),
                ),
                "run_sub_agent_adaptationStrategy" => (
                    "scriptAgent:adaptationStrategyAgent",
                    "改编策略",
                    Some("adaptationStrategy"),
                ),
                "run_sub_agent_script" => ("scriptAgent:scriptAgent", "剧本", None),
                "run_supervision_agent" => ("scriptAgent:supervisionAgent", "监督", None),
                _ => return Err(AppError::bad_request("不支持的剧本子 Agent")),
            };
            let prompt = request
                .arguments
                .get("prompt")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("缺少 prompt"))?;
            let supervision_episode_limit = if agent_key == "scriptAgent:supervisionAgent" {
                let workspace: Option<Value> = sqlx::query_scalar(
                    "SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id IS NULL AND key='scriptAgent'",
                )
                .bind(request.project_id)
                .fetch_optional(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to load script supervision scope"))?;
                let strategy = workspace
                    .as_ref()
                    .and_then(|data| data.get("adaptationStrategy"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                episode_limit(strategy, prompt)
            } else {
                None
            };
            let system = toonflow_agent_runtime::load_agent_skill(&state.pool, agent_key)
                .await
                .map_err(AppError::bad_request)?;

            // Create sub-agent message bubble if emitter is available
            let sub_msg = if let Some(ref emitter) = request.emitter {
                let sub_label = match label {
                    "监督" => "编辑",
                    _ => "编剧",
                };
                let (mid, _) = emitter.new_message(sub_label, "assistant");
                let cid = emitter.add_content(&mid, "text", &json!(""));
                Some((mid, cid, emitter.clone()))
            } else {
                None
            };

            // Build project context
            let project_hint: String = sqlx::query_scalar(
                "SELECT '作品名：'||name||'\n小说类型：'||type||'\n小说简介：'||COALESCE(intro,'无')||'\n视觉风格：'||COALESCE(art_style,'未设置')||'\n视频画幅：'||COALESCE(video_ratio,'16:9') FROM toonflow.projects WHERE id=$1",
            )
            .bind(request.project_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::bad_request("无法加载项目信息"))?
            .unwrap_or_default();
            let supervision_scope = supervision_episode_limit
                .map(|limit| format!(
                    "\n\n## 剧集范围硬约束\n当前改编只包含第1-{limit}集。审核报告只能引用第1-{limit}集；严禁引用、推测或规划第{}集及以后。必须明确区分“原著第X章”与“短剧第X集”，不得将章号改写为集号。",
                    limit + 1
                ))
                .unwrap_or_default();
            let format_instruction = match tag {
                Some(tag) => workspace_format_instruction(tag, label),
                None if agent_key == "scriptAgent:scriptAgent" => {
                    script_format_instruction().to_string()
                }
                _ => String::new(),
            };
            let workspace_snapshot: Option<Value> = sqlx::query_scalar(
                "SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id IS NULL AND key='scriptAgent'",
            )
            .bind(request.project_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to preload script workspace"))?;
            let workspace_context = workspace_snapshot
                .as_ref()
                .map(Value::to_string)
                .unwrap_or_else(|| "（当前工作区为空）".to_string());
            let full_system = format!(
                "{system}\n\n## 当前项目\n{project_hint}\n\n## 服务端预载的最新剧本工作区\n{workspace_context}\n\n你是 Toonflow 的{label}子 Agent。需要核实原著时使用读取工具；完成后直接输出一次最终 XML，不要先输出草稿。{supervision_scope}"
            ) + &format_instruction + crate::toonflow_script_quality::POLICY;
            let has_novel: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM toonflow.novels WHERE project_id=$1)",
            )
            .bind(request.project_id)
            .fetch_one(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to inspect source availability"))?;
            let quality_stage = matches!(agent_key,
                "scriptAgent:storySkeletonAgent" | "scriptAgent:adaptationStrategyAgent" | "scriptAgent:supervisionAgent");
            let mut evidence = crate::toonflow_script_quality::ReadEvidence::default();
            evidence.workspace = true;
            let mut correction_attempts = 0;

            // Sub-agent read-only tool definitions
            let sub_tools: Vec<Value> = vec![
                json!({"type":"function","function":{"name":"get_novel_events","description":"获取项目章节事件列表","parameters":{"type":"object","properties":{"chapterIndexs":{"type":"array","items":{"type":"number"},"description":"章节编号列表"}},"required":["chapterIndexs"]}}}),
                json!({"type":"function","function":{"name":"get_novel_text","description":"获取指定章节的原始文本","parameters":{"type":"object","properties":{"chapterIndex":{"type":"number","description":"章节编号"}},"required":["chapterIndex"]}}}),
                json!({"type":"function","function":{"name":"get_planData","description":"读取工作区已有数据（故事骨架、改编策略等）","parameters":{"type":"object","properties":{"key":{"type":"string","description":"数据key"}},"required":["key"]}}}),
                json!({"type":"function","function":{"name":"get_script_content","description":"读取已有剧本内容","parameters":{"type":"object","properties":{"ids":{"type":"array","items":{"type":"number"},"description":"剧本ID列表"}},"required":["ids"]}}}),
            ];

            // Run sub-agent with native function calling
            let mut messages = vec![json!({"role":"system","content":full_system})];
            if agent_key == "scriptAgent:scriptAgent" {
                let scripts: Vec<(i64, String)> = sqlx::query_as(
                    "SELECT id,name FROM toonflow.scripts WHERE project_id=$1 ORDER BY create_time,id",
                )
                .bind(request.project_id)
                .fetch_all(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to load existing script context"))?;
                let chapter_count: i64 =
                    sqlx::query_scalar("SELECT count(*) FROM toonflow.novels WHERE project_id=$1")
                        .bind(request.project_id)
                        .fetch_one(&state.pool)
                        .await
                        .map_err(|_| AppError::internal("failed to load novel chapter count"))?;
                let list = scripts
                    .iter()
                    .map(|(id, name)| format!("{id}:{}", name.replace([',', ':'], "")))
                    .collect::<Vec<_>>()
                    .join(",");
                let latest = scripts
                    .last()
                    .map(|(id, _)| id.to_string())
                    .unwrap_or_else(|| "无".into());
                messages.push(json!({"role":"assistant","content":format!("## 可用剧本（ID:名称）\n{list}\n最新一集 ID：{latest}\n章节数量：{chapter_count}章")}));
            }
            messages.push(json!({"role":"user","content":format!("{prompt}{format_instruction}")}));
            let mut output = String::new();
            for _round in 0..24 {
                let raw = ai_client::project_text_tools(
                    &state.pool,
                    agent_key,
                    request.project_id,
                    messages.clone(),
                    sub_tools.clone(),
                )
                .await
                .map_err(AppError::bad_request)?;
                let message = raw
                    .pointer("/choices/0/message")
                    .cloned()
                    .ok_or_else(|| AppError::bad_request("模型响应缺少 message"))?;
                let calls = message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if calls.is_empty() {
                    let candidate = message
                        .get("content")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    if candidate.is_empty() {
                        return Err(AppError::bad_request("子 Agent 未返回有效内容"));
                    }
                    if quality_stage {
                        if let Some(reason) = evidence.missing(has_novel, true) {
                            correction_attempts += 1;
                            if correction_attempts > 2 {
                                return Err(AppError::bad_request(format!("原著核实未完成，未保存本次生成：{reason}")));
                            }
                            messages.push(message);
                            messages.push(json!({"role":"user","content":reason}));
                            continue;
                        }
                    }
                    if let Some(limit) = supervision_episode_limit {
                        let invalid = explicit_episode_limit(&candidate)
                            .filter(|episode| *episode > limit)
                            .into_iter()
                            .collect::<Vec<_>>();
                        if !invalid.is_empty() {
                            messages.push(message);
                            messages.push(json!({
                                "role":"user",
                                "content":format!(
                                    "上一版审核报告越界引用了第{}集。当前只允许第1-{limit}集。请立即重写完整报告，删除所有超范围集数，并将原著章号明确写为“原著第X章”。不得解释错误。",
                                    invalid.iter().map(u32::to_string).collect::<Vec<_>>().join("、")
                                )
                            }));
                            continue;
                        }
                    }
                    output = candidate;
                    break;
                }
                messages.push(message);
                for call in &calls {
                    let call_id = call.get("id").and_then(Value::as_str).unwrap_or("");
                    let tool_name = call
                        .pointer("/function/name")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let args = call
                        .pointer("/function/arguments")
                        .and_then(Value::as_str)
                        .unwrap_or("{}");
                    let args: Value = serde_json::from_str(args).unwrap_or(json!({}));
                    let result = toonflow_agent_read_tools::execute_sub_tool(
                        state,
                        request.project_id,
                        tool_name,
                        &args,
                    )
                    .await;
                    evidence.record(tool_name, &args, &result);
                    messages.push(json!({"role":"tool","tool_call_id":call_id,"content":result}));
                }
            }
            if output.is_empty() {
                if let Some((mid, _, emitter)) = &sub_msg {
                    emitter.update_message(mid, "error", Some("子 Agent 工具调用超过最大轮数"));
                }
                return Err(AppError::bad_request("子 Agent 工具调用超过最大轮数"));
            }

            let memory_role = if agent_key.ends_with(":supervisionAgent") {
                "assistant:supervision".to_string()
            } else {
                format!(
                    "assistant:execution:{}",
                    agent_key.rsplit(':').next().unwrap_or(agent_key)
                )
            };
            toonflow_agents::add_memory(
                state,
                &request.agent_type,
                &request.isolation_key,
                &memory_role,
                &toonflow_agent_runtime::strip_xml_tags(&output),
            )
            .await?;

            // Complete sub-agent message bubble
            if let Some((mid, cid, emitter)) = &sub_msg {
                emitter.text_delta(mid, cid, &output);
                emitter.text_complete(mid, cid);
                emitter.update_message(mid, "complete", None);
            }
            if let Some(tag) = tag {
                let content = required_tagged(&output, tag)?;
                let mut data:Value=sqlx::query_scalar("SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id IS NULL AND key='scriptAgent'").bind(request.project_id).fetch_optional(&state.pool).await.map_err(|_|AppError::internal("failed to load script workspace"))?.unwrap_or_else(||json!({"storySkeleton":"","adaptationStrategy":""}));
                data[tag] = json!(content);
                data[format!("{tag}Evidence")] = json!({
                    "sourceChaptersRead": evidence.chapters,
                    "workspaceRead": evidence.workspace,
                    "singlePassQualityPolicy": quality_stage,
                    "savedAt": now_ms(),
                    "policyVersion": 2
                });
                sqlx::query("INSERT INTO toonflow.agent_work_data(project_id,episodes_id,key,data,create_time,update_time)VALUES($1,NULL,'scriptAgent',$2,$3,$3) ON CONFLICT(project_id,key) WHERE episodes_id IS NULL DO UPDATE SET data=excluded.data,update_time=excluded.update_time").bind(request.project_id).bind(data).bind(now_ms()).execute(&state.pool).await.map_err(|_|AppError::internal("failed to save sub agent result"))?;
            }
            Ok(json!({"agent":agent_key,"content":output,
                "evidence":{"sourceChaptersRead":evidence.chapters,"workspaceRead":evidence.workspace,"singlePassQualityPolicy":quality_stage}}))
        }
        ("productionAgent", "get_flowData") => {
            let script_id = request
                .script_id
                .ok_or_else(|| AppError::bad_request("生产工具缺少 scriptId"))?;
            let key = request
                .arguments
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if matches!(key, "script" | "assets") {
                let (script, assets) = crate::toonflow_asset_context::load_script_context(
                    &state.pool,
                    request.project_id,
                    script_id,
                )
                .await
                .map_err(|_| AppError::internal("failed to build production asset context"))?;
                let value = if key == "script" {
                    json!(script)
                } else {
                    assets
                };
                return Ok(changed_flow_data(&state.pool, &request.isolation_key, key, value).await);
            }
            if key == "storyboard" {
                let data = crate::toonflow_video::load_generate_data(
                    &state.pool,
                    request.project_id,
                    script_id,
                )
                .await?;
                return Ok(changed_flow_data(
                    &state.pool,
                    &request.isolation_key,
                    key,
                    data["storyboardList"].clone(),
                )
                .await);
            }
            let data:Option<Value>=sqlx::query_scalar("SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id=$2 AND key='productionAgent'").bind(request.project_id).bind(script_id).fetch_optional(&state.pool).await.map_err(|_|AppError::internal("failed to get flow data"))?;
            let data = data.unwrap_or_else(|| json!({}));
            let value = if key.is_empty() {
                data
            } else {
                data.get(key).cloned().unwrap_or(Value::Null)
            };
            Ok(changed_flow_data(&state.pool, &request.isolation_key, key, value).await)
        }
        ("productionAgent", "get_video_workbench") => {
            let script_id = request
                .script_id
                .ok_or_else(|| AppError::bad_request("视频工具缺少 scriptId"))?;
            crate::toonflow_video::load_generate_data(&state.pool, request.project_id, script_id)
                .await
        }
        ("productionAgent", "generate_video_prompt") => {
            let track_id = request
                .arguments
                .get("trackId")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 trackId"))?;
            let setting: Option<(Option<i64>, String)> =
                sqlx::query_as("SELECT video_model,mode FROM toonflow.projects WHERE id=$1")
                    .bind(request.project_id)
                    .fetch_optional(&state.pool)
                    .await
                    .map_err(|_| AppError::internal("failed to load video settings"))?;
            let (model, mode) = setting.ok_or_else(|| AppError::not_found("project not found"))?;
            let model = model.ok_or_else(|| AppError::bad_request("项目未配置视频模型"))?;
            let prompt = crate::toonflow_video::create_prompt(
                &state.pool,
                track_id,
                request.project_id,
                &model.to_string(),
                &mode,
            )
            .await
            .map_err(AppError::bad_request)?;
            Ok(json!({"trackId":track_id,"prompt":prompt}))
        }
        ("productionAgent", "update_video_prompt") => {
            let track_id = request
                .arguments
                .get("trackId")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 trackId"))?;
            let prompt = request
                .arguments
                .get("prompt")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("缺少 prompt"))?;
            let result = sqlx::query(
                "UPDATE toonflow.video_tracks SET prompt=$3 WHERE id=$1 AND project_id=$2",
            )
            .bind(track_id)
            .bind(request.project_id)
            .bind(prompt)
            .execute(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to update video prompt"))?;
            if result.rows_affected() == 0 {
                return Err(AppError::not_found("video track not found"));
            }
            Ok(json!({"trackId":track_id,"prompt":prompt}))
        }
        ("productionAgent", "select_video") => {
            let track_id = request
                .arguments
                .get("trackId")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 trackId"))?;
            let video_id = request
                .arguments
                .get("videoId")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 videoId"))?;
            let result=sqlx::query("UPDATE toonflow.video_tracks t SET video_id=$3 WHERE t.id=$1 AND t.project_id=$2 AND EXISTS(SELECT 1 FROM toonflow.videos v WHERE v.id=$3 AND v.video_track_id=t.id AND v.state='生成成功')").bind(track_id).bind(request.project_id).bind(video_id).execute(&state.pool).await.map_err(|_|AppError::internal("failed to select video"))?;
            if result.rows_affected() == 0 {
                return Err(AppError::bad_request("轨道或成功视频不存在"));
            }
            Ok(json!({"trackId":track_id,"videoId":video_id}))
        }
        ("productionAgent", "add_deriveAsset") => {
            let parent = request
                .arguments
                .get("assetsId")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 assetsId"))?;
            let appearance_id = request
                .arguments
                .get("appearanceId")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 appearanceId，禁止临时编写服装"))?;
            let script_id = request
                .script_id
                .ok_or_else(|| AppError::bad_request("人物造型写入缺少 scriptId"))?;
            let appearance: Option<(String, String)> = sqlx::query_as(
                "SELECT name,costume_prompt FROM toonflow.character_appearances WHERE id=$1 AND project_id=$2 AND script_id=$3 AND role_asset_id=$4",
            )
            .bind(appearance_id)
            .bind(request.project_id)
            .bind(script_id)
            .bind(parent)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get character appearance"))?;
            let (appearance_name, costume_prompt) = appearance.ok_or_else(|| {
                AppError::bad_request("appearanceId 与当前剧本人物不匹配，请重新读取 assets")
            })?;
            let parent_type: Option<String> = sqlx::query_scalar(
                "SELECT type FROM toonflow.assets WHERE id=$1 AND project_id=$2",
            )
            .bind(parent)
            .bind(request.project_id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to get parent asset"))?;
            let existing_id: Option<i64> = sqlx::query_scalar(
                r#"SELECT id FROM toonflow.assets
                   WHERE project_id=$1 AND parent_asset_id=$3
                     AND (appearance_id=$2 OR (
                       appearance_id IS NULL AND name=$4 AND description=$5
                     ))
                   ORDER BY appearance_id NULLS LAST LIMIT 1"#,
            )
            .bind(request.project_id)
            .bind(appearance_id)
            .bind(parent)
            .bind(&appearance_name)
            .bind(&costume_prompt)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to find appearance derivative"))?;
            let id = existing_id.unwrap_or_else(|| {
                request
                    .arguments
                    .get("id")
                    .and_then(Value::as_i64)
                    .unwrap_or(now_ms() * 1000)
            });
            let parent_type =
                parent_type.ok_or_else(|| AppError::not_found("parent asset not found"))?;
            if parent_type != "role" {
                return Err(AppError::bad_request(
                    "只有人物资产需要创建衍生图；场景变化请在分镜阶段生成",
                ));
            }
            sqlx::query("INSERT INTO toonflow.assets(id,name,prompt,prompt_state,type,description,parent_asset_id,appearance_id,project_id,start_time)VALUES($1,$2,$3,'已完成',$4,$3,$5,$6,$7,$8) ON CONFLICT(id) DO UPDATE SET name=excluded.name,description=excluded.description,prompt=excluded.prompt,prompt_state='已完成',prompt_error_reason=NULL,appearance_id=excluded.appearance_id,image_id=CASE WHEN toonflow.assets.description IS DISTINCT FROM excluded.description THEN NULL ELSE toonflow.assets.image_id END")
                .bind(id).bind(&appearance_name).bind(&costume_prompt).bind(parent_type).bind(parent).bind(appearance_id).bind(request.project_id).bind(now_ms()).execute(&state.pool).await.map_err(|_|AppError::internal("failed to save derived asset"))?;
            sqlx::query("INSERT INTO toonflow.script_assets(script_id,asset_id)VALUES($1,$2) ON CONFLICT DO NOTHING").bind(script_id).bind(id).execute(&state.pool).await.ok();
            Ok(json!({"id":id,"appearanceId":appearance_id,"name":appearance_name}))
        }
        ("productionAgent", "del_deriveAsset") => {
            let id = request
                .arguments
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 id"))?;
            sqlx::query("DELETE FROM toonflow.assets WHERE id=$1 AND project_id=$2 AND parent_asset_id IS NOT NULL").bind(id).bind(request.project_id).execute(&state.pool).await.map_err(|_|AppError::internal("failed to delete derived asset"))?;
            Ok(json!(true))
        }
        ("productionAgent", "generate_deriveAsset") => {
            let mut ids = request
                .arguments
                .get("ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_i64())
                .collect::<Vec<_>>();
            ids.sort_unstable();
            ids.dedup();
            if ids.is_empty() {
                return Err(AppError::bad_request("ids不能为空"));
            }
            let valid_ids: Vec<i64> = sqlx::query_scalar(
                "SELECT id FROM toonflow.assets WHERE project_id=$1 AND id=ANY($2) AND type='role' AND parent_asset_id IS NOT NULL",
            )
            .bind(request.project_id)
            .bind(&ids)
            .fetch_all(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to validate derived assets"))?;
            if valid_ids.len() != ids.len() {
                return Err(AppError::bad_request(
                    "generate_deriveAsset 只能生成有父资产的人物衍生图，禁止传入人物/场景/道具基础资产",
                ));
            }
            let rows = toonflow_asset_ai::schedule_and_wait_asset_generation(
                &state.pool,
                request.project_id,
                &valid_ids,
                request
                    .arguments
                    .get("concurrentCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(3) as usize,
            )
            .await?;
            Ok(json!(rows))
        }
        ("productionAgent", name) if name.starts_with("run_sub_agent_") => {
            let (agent_key, label, flow_tag, allowed_tools): (_, _, _, &[&str]) = match name {
                "run_sub_agent_derive_assets" => (
                    "productionAgent:deriveAssetsAgent",
                    "衍生资产",
                    None,
                    &[
                        "get_flowData",
                        "add_deriveAsset",
                        "del_deriveAsset",
                        "use_skill",
                        "read_skill_file",
                    ],
                ),
                "run_sub_agent_generate_assets" => (
                    "productionAgent:generateAssetsAgent",
                    "资产生成",
                    None,
                    &[
                        "get_flowData",
                        "generate_deriveAsset",
                        "use_skill",
                        "read_skill_file",
                    ],
                ),
                "run_sub_agent_director_plan" => (
                    "productionAgent:directorPlanAgent",
                    "导演规划",
                    Some(("scriptPlan", "scriptPlan")),
                    &[
                        "get_flowData",
                        "set_flowData",
                        "use_skill",
                        "read_skill_file",
                    ],
                ),
                "run_sub_agent_storyboard_gen" => (
                    "productionAgent:storyboardGenAgent",
                    "分镜图生成",
                    None,
                    &[
                        "get_flowData",
                        "update_storyboard",
                        "generate_storyboard",
                        "use_skill",
                        "read_skill_file",
                    ],
                ),
                "run_sub_agent_image_edit" => (
                    "productionAgent:storyboardGenAgent",
                    "图片编辑规划",
                    None,
                    &["get_flowData", "use_skill", "read_skill_file"],
                ),
                "run_sub_agent_storyboard_panel" => (
                    "productionAgent:storyboardPanelAgent",
                    "分镜面板",
                    None,
                    &[
                        "get_flowData",
                        "add_flowData_storyboard",
                        "update_storyboard",
                        "use_skill",
                        "read_skill_file",
                    ],
                ),
                "run_sub_agent_storyboard_table" => (
                    "productionAgent:storyboardTableAgent",
                    "分镜表",
                    Some(("storyboardTable", "storyboardTable")),
                    STORYBOARD_TABLE_AGENT_TOOLS,
                ),
                "run_sub_agent_supervision" => (
                    "productionAgent:supervisionAgent",
                    "监制",
                    None,
                    &["get_flowData", "use_skill", "read_skill_file"],
                ),
                _ => return Err(AppError::bad_request("不支持的生产子 Agent")),
            };
            let prompt = request
                .arguments
                .get("prompt")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("缺少 prompt"))?;
            if agent_key == "productionAgent:storyboardTableAgent" {
                let script_id = request
                    .script_id
                    .ok_or_else(|| AppError::bad_request("分镜表生成缺少 scriptId"))?;
                let (script, assets) = crate::toonflow_asset_context::load_script_context(
                    &state.pool,
                    request.project_id,
                    script_id,
                )
                .await
                .map_err(|_| AppError::internal("failed to load storyboard planning context"))?;
                if script.trim().is_empty() {
                    return Err(AppError::bad_request(
                        "分镜表生成前置检查失败：当前剧本正文为空",
                    ));
                }
                if assets.as_array().is_none_or(Vec::is_empty) {
                    return Err(AppError::bad_request(
                        "分镜表生成前置检查失败：当前剧本尚未关联可引用资产",
                    ));
                }
                let data: Option<Value> = sqlx::query_scalar("SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id=$2 AND key='productionAgent'")
                    .bind(request.project_id).bind(script_id).fetch_optional(&state.pool).await
                    .map_err(|_| AppError::internal("failed to load storyboard planning workspace"))?;
                let director_plan = data
                    .as_ref()
                    .and_then(|value| value.get("scriptPlan"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if director_plan.trim().is_empty() {
                    return Err(AppError::bad_request(
                        "分镜表生成前置检查失败：请先完成导演规划",
                    ));
                }
            }
            let storyboard_panel_validation = if agent_key == "productionAgent:storyboardPanelAgent"
            {
                let script_id = request
                    .script_id
                    .ok_or_else(|| AppError::bad_request("分镜面板写入缺少 scriptId"))?;
                let mode: String =
                    sqlx::query_scalar("SELECT mode FROM toonflow.projects WHERE id=$1")
                        .bind(request.project_id)
                        .fetch_optional(&state.pool)
                        .await
                        .map_err(|_| AppError::internal("failed to load storyboard panel mode"))?
                        .unwrap_or_else(|| "text".to_string());
                let image_model: String = sqlx::query_scalar(
                    "SELECT lower(coalesce(m.name,'') || ' ' || coalesce(m.key,'') || ' ' || coalesce(m.model,'')) FROM toonflow.projects p LEFT JOIN ai.model_configs m ON m.id=p.image_model WHERE p.id=$1",
                )
                .bind(request.project_id)
                .fetch_optional(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to load storyboard image model"))?
                .unwrap_or_default();
                let prompt_format =
                    crate::toonflow_storyboard_panel_validation::prompt_format(&image_model);
                let data: Option<Value> = sqlx::query_scalar("SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id=$2 AND key='productionAgent'")
                    .bind(request.project_id).bind(script_id).fetch_optional(&state.pool).await
                    .map_err(|_| AppError::internal("failed to load storyboard table for panel"))?;
                let table = data
                    .as_ref()
                    .and_then(|value| value.get("storyboardTable"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if table.trim().is_empty() {
                    return Err(AppError::bad_request(
                        "分镜面板写入前置检查失败：请先完成分镜表",
                    ));
                }
                let first_frame = mode != "text";
                let derived_role_aliases: std::collections::HashMap<i64, Vec<String>> =
                    sqlx::query_as::<_, (i64, String)>(
                        r#"SELECT child.id, parent.name
                           FROM toonflow.assets child
                           JOIN toonflow.assets parent ON parent.id=child.parent_asset_id
                           WHERE child.project_id=$1
                             AND child.type='role' AND parent.type='role'
                             AND EXISTS(
                               SELECT 1 FROM toonflow.script_assets linked
                               WHERE linked.script_id=$2
                                 AND linked.asset_id IN (child.id,parent.id)
                             )"#,
                    )
                    .bind(request.project_id)
                    .bind(script_id)
                    .fetch_all(&state.pool)
                    .await
                    .map_err(|_| AppError::internal("failed to load derived role aliases"))?
                    .into_iter()
                    .fold(
                        std::collections::HashMap::new(),
                        |mut aliases, (id, name)| {
                            aliases.entry(id).or_default().push(name);
                            aliases
                        },
                    );
                let expected =
                    crate::toonflow_storyboard_panel_validation::expected_items_with_aliases(
                        table,
                        first_frame,
                        &derived_role_aliases,
                    );
                if expected.is_empty() {
                    return Err(AppError::bad_request(
                        "分镜面板写入前置检查失败：分镜表中没有可识别的写入单位",
                    ));
                }
                Some((first_frame, prompt_format, expected))
            } else {
                None
            };
            if matches!(
                name,
                "run_sub_agent_derive_assets" | "run_sub_agent_generate_assets"
            ) {
                let script_id = request
                    .script_id
                    .ok_or_else(|| AppError::bad_request("人物造型流程缺少 scriptId"))?;
                let missing_extraction =
                    role_names_without_appearances(&state.pool, request.project_id, script_id)
                        .await?;
                if !missing_extraction.is_empty() {
                    return Err(AppError::bad_request(format!(
                        "AI 资产提取尚未保存以下人物的场景服装提示词：{}。请先回到剧本资产阶段重新执行 AI 资产提取。",
                        missing_extraction.join("、")
                    )));
                }
                if name == "run_sub_agent_generate_assets" {
                    let missing =
                        missing_appearance_derivatives(&state.pool, request.project_id, script_id)
                            .await?;
                    if !missing.is_empty() {
                        return Err(AppError::bad_request(format!(
                            "不能生成衍生图片：以下已提取造型尚未创建衍生人物：{}。请先执行人物衍生资产分析。",
                            missing.join("、")
                        )));
                    }
                }
            }
            let system = toonflow_agent_runtime::load_agent_skill(&state.pool, agent_key)
                .await
                .map_err(AppError::bad_request)?;
            let project_hint =
                toonflow_agents::production_context(state, request.project_id, request.script_id)
                    .await?;
            let generation_gate = if agent_key == "productionAgent:storyboardTableAgent" {
                "\n\n## Toonflow 分镜规划强制执行顺序（不得跳步）\n1. 首轮只调用 get_flowData，依次读取 script、assets、scriptPlan；三项必须全部实际读取，禁止依靠记忆补写。\n2. 先在回复中输出简短的逐场结构化草案：逐条台词按4字/秒估时、划分不超过15秒的片段、写明相邻片段的桥梁元素、标出长台词拆镜点并核对全员视觉落点。\n3. 草案完成后保存完整 storyboardTable；标签内部必须是技能模板规定的 Markdown，禁止 JSON、XML 子标签和代码围栏。\n4. 每一行镜头必须能直接生成一张构图明确的静态关键帧：只允许一个时间点、一个机位、一个连续动作状态。禁止蒙太奇、快切、多景别、定格画面、用箭头串联多个动作或在同一行跨时间。\n5. 逐场维护“在场角色状态”：跨片段持续记录每个人物的入场、位置、基础姿态、承托物、朝向、持有物和离场。相邻镜头仍在同一空间且没有明确离场、转场、特写、反打或画外依据时，上一镜在场人物必须继续写入画面描述并绑定对应衍生资产；不得因本镜没有台词或主动作而让人物凭空消失。\n6. 画面描述必须逐镜展开本镜所有出镜人物的姓名、基础姿态、承托物、位置、朝向及相互空间关系；禁止用“同前”“保持原姿势”或上下文隐含替代。即使某人没有主动作，只要同框也必须明确描述，例如病床对话每一镜都要写患者仰躺在病床上、陪伴者坐在床边。只有明确写出坐起、下床、起身、站起等可见动作时才能改变既有姿态。\n7. 画面描述、运镜、音效不得出现光影色调词；画面描述不得重复服装、发型、五官、肤色等资产固有外观。\n8. 每镜含台词最低时长按：台词字数÷4 + 每处标点停顿0.4秒 + 1秒安全余量，最终向上取整；台词必须与剧本逐字一致。\n9. 只有本镜实际出现在画面中的人物、场景和物件才能绑定；每个已绑定资产都必须在该镜画面描述中明确出现，禁止把片段级资产整组复制到每一镜。\n10. 人物在当前场次存在 scenes 匹配的衍生形象时，必须引用该衍生资产的名称和ID，禁止继续引用基础人物。输出前逐镜自检，任一项不满足不得输出。"
            } else {
                ""
            };
            let document_format_gate = if agent_key == "productionAgent:directorPlanAgent" {
                "\n\n## 输出格式强制要求\n<scriptPlan> 标签内部必须直接使用技能模板规定的 Markdown 表格、标题和列表。严禁输出 ```xml 代码围栏，严禁使用 <sceneSummaryTable>、<sceneNotes>、<scene>、<note> 等 XML 子标签，也不得把正文包装成 JSON 对象。"
            } else {
                ""
            };
            let storyboard_panel_mode_gate = if agent_key == "productionAgent:storyboardPanelAgent"
            {
                if storyboard_panel_validation
                    .as_ref()
                    .is_some_and(|(first_frame, _, _)| !first_frame)
                {
                    "\n\n## 本次写入路由（服务端已确定）\n必须执行“纯文本多参模式”：以分镜表片段为写入单位，prompt 传 null，shouldGenerateImage 传 false。禁止自行切换模式。"
                } else {
                    match storyboard_panel_validation.as_ref().map(|value| value.1) {
                        Some(
                            crate::toonflow_storyboard_panel_validation::PromptFormat::Seedream,
                        ) => {
                            "\n\n## 本次写入路由（服务端已确定）\n必须执行“首位帧模式”与 Seedream 模式A：分镜表每一行独立写入；prompt 使用中文【画面】【风格】结构，按关联资产顺序声明 @图N，并在【画面】正文用 @图N 替换对应资产名称。每个角色 @图N 都必须作为主语单独写出其基础姿态、承托物、位置和朝向；“@图2 盯着 @图1”不能代替 @图1 自身的姿态描述。相邻镜头没有明确起身/坐下等变化时，必须把上一镜姿态逐字展开到本镜；人物参考图仅锁定身份和服装，禁止继承其站立设定姿态。prompt 只写可视画面，禁止写入台词、对白、音效或要求画面内字幕；shouldGenerateImage 传 true。禁止输出 JSON，禁止自行切换模式。"
                        }
                        Some(
                            crate::toonflow_storyboard_panel_validation::PromptFormat::Nanobanana,
                        ) => {
                            "\n\n## 本次写入路由（服务端已确定）\n必须执行“首位帧模式”与 Nanobanana 模式B：分镜表每一行独立写入；prompt 使用包含 character_reference、continuity_rules、shot、negative 的英文 JSON 结构，每个角色 @图N 必须在参考声明及 shot 正文中出现，并在自己的 shot 状态中明确 base pose、support surface、position 和 facing；作为另一角色 gaze target 不算自身状态。没有明确姿态转换时逐镜继承并展开上一镜姿态；人物参考图只锁定身份和服装，不继承站姿。shouldGenerateImage 传 true。禁止自行切换模式。"
                        }
                        _ => {
                            "\n\n## 本次写入路由（服务端已确定）\n必须执行“首位帧模式”：分镜表每一行独立写入，生成忠实的静态首帧 prompt，按关联资产顺序建立 @图N 绑定；每个角色 @图N 都必须独立写明基础姿态和承托物，并逐镜展开未发生变化的上一镜姿态，不能只作为其他角色动作的宾语出现。人物参考图只锁定身份和服装，不继承站姿。shouldGenerateImage 传 true。禁止自行切换模式。"
                        }
                    }
                }
            } else {
                ""
            };
            let storyboard_generation_gate = if agent_key == "productionAgent:storyboardGenAgent" {
                "\n\n## 分镜图生成强制流程\n1. 必须先调用 get_flowData 读取 storyboard 的最新分镜、状态、失败原因和资产绑定。\n2. 如果失败原因是已绑定 @图N 未描述或角色 @图N 缺少自身姿态，必须逐条核对本镜真实可见对象及上一镜角色状态，调用 update_storyboard 同时修正 prompt、videoDesc 和 associateAssetsIds。每个出镜角色必须作为主语写出基础姿态与承托物；“另一人物盯着 @图N”不能代替该角色自身描述。不可把本镜不出镜的人物强行写进提示词。\n3. 修复后才能调用 generate_storyboard；只提交委派范围或用户明确选中的 ID，禁止自动扩大为全部分镜。\n4. concurrentCount 默认使用 2，避免图片模型限流。"
            } else {
                ""
            };
            let role_binding_gate = if matches!(
                agent_key,
                "productionAgent:storyboardTableAgent" | "productionAgent:storyboardPanelAgent"
            ) {
                "\n\n## 人物资产硬性规则\n基础人物只是衍生关系的母资产，一律禁止写入分镜引用。画面中只要出现人物，必须按当前场次的 scenes 匹配并引用对应衍生人物的名称和 ID。如果没有匹配的衍生形象，必须停止写入并先补齐衍生资产，不得回退使用基础人物。"
            } else {
                ""
            };
            let sub_system = format!(
                "{system}\n\n你是 Toonflow 的{label}子 Agent。严格完成委派任务并实际调用要求的工具，不得只用文字声称完成。{generation_gate}{document_format_gate}{storyboard_panel_mode_gate}{storyboard_generation_gate}{role_binding_gate}\n\n{project_hint}"
            );
            let scoped_run = Box::pin(toonflow_agents::run_scoped_production_agent_with_emitter(
                state,
                agent_key,
                &sub_system,
                prompt,
                request.project_id,
                request.script_id,
                allowed_tools,
                request.emitter.as_ref(),
            ));
            let panel_batch_context = (agent_key == "productionAgent:storyboardPanelAgent")
                .then(|| std::sync::Arc::new(StoryboardPanelBatchContext::default()));
            let mut panel_batch_guard = panel_batch_context.as_ref().map(|context| {
                StoryboardPanelBatchGuard::new(
                    state.pool.clone(),
                    request.project_id,
                    request
                        .script_id
                        .expect("storyboard panel scriptId checked above"),
                    context.clone(),
                )
            });
            let run_result = if let Some(context) = &panel_batch_context {
                DEFER_STORYBOARD_TRANSITION_SYNC
                    .scope(context.clone(), scoped_run)
                    .await
            } else {
                scoped_run.await
            };
            let mut output = match run_result {
                Ok(output) => output,
                Err(error) => {
                    if let (Some(context), Some(script_id)) =
                        (&panel_batch_context, request.script_id)
                    {
                        rollback_storyboard_panel_batch(
                            &state.pool,
                            request.project_id,
                            script_id,
                            &context.created_ids(),
                        )
                        .await?;
                        if let Some(guard) = panel_batch_guard.as_mut() {
                            guard.disarm();
                        }
                    }
                    return Err(error);
                }
            };
            if let Some((first_frame, prompt_format, expected)) = &storyboard_panel_validation {
                let script_id = request.script_id.expect("panel scriptId checked above");
                let context = panel_batch_context
                    .as_ref()
                    .expect("storyboard panel batch context initialized above");
                let touched_ids = context.touched_ids();
                let created_ids = context.created_ids();
                let rows: Vec<(i64, String, String, String, i32, String)> = sqlx::query_as(
                    "SELECT id,prompt,coalesce(track,''),coalesce(duration,'0'),should_generate_image,coalesce(video_desc,'')
                     FROM toonflow.storyboards
                     WHERE project_id=$1 AND script_id=$2 AND id=ANY($3)
                     ORDER BY index,id",
                )
                .bind(request.project_id)
                .bind(script_id)
                .bind(&touched_ids)
                .fetch_all(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to validate storyboard panel rows"))?;
                let new_rows = rows
                    .iter()
                    .filter(|row| created_ids.contains(&row.0))
                    .cloned()
                    .collect::<Vec<_>>();
                let mut actual = Vec::with_capacity(rows.len());
                for (id, prompt, track, duration, should_generate_image, video_desc) in &rows {
                    let asset_ids: Vec<i64> = sqlx::query_scalar(
                        "SELECT asset_id FROM toonflow.assets_storyboards WHERE storyboard_id=$1 ORDER BY sort_order,asset_id",
                    )
                    .bind(id)
                    .fetch_all(&state.pool)
                    .await
                    .map_err(|_| AppError::internal("failed to validate storyboard assets"))?;
                    actual.push(
                        crate::toonflow_storyboard_panel_validation::ActualPanelItem {
                            prompt: prompt.clone(),
                            video_desc: video_desc.clone(),
                            track: track.clone(),
                            duration: duration.parse().unwrap_or_default(),
                            should_generate_image: *should_generate_image != 0,
                            asset_ids,
                        },
                    );
                }
                let expected_asset_ids = expected
                    .iter()
                    .flat_map(|item| item.asset_ids.iter().copied())
                    .collect::<Vec<_>>();
                let role_asset_ids: std::collections::HashSet<i64> = sqlx::query_scalar(
                    "SELECT id FROM toonflow.assets WHERE id=ANY($1) AND type='role'",
                )
                .bind(&expected_asset_ids)
                .fetch_all(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to load storyboard role assets"))?
                .into_iter()
                .collect();
                let issues = crate::toonflow_storyboard_panel_validation::validate(
                    expected,
                    &actual,
                    *first_frame,
                    *prompt_format,
                    &role_asset_ids,
                );
                if !issues.is_empty() {
                    rollback_storyboard_panel_batch(
                        &state.pool,
                        request.project_id,
                        script_id,
                        &created_ids,
                    )
                    .await?;
                    if let Some(guard) = panel_batch_guard.as_mut() {
                        guard.disarm();
                    }
                    return Err(AppError::bad_request(format!(
                        "分镜面板写入未通过 Toonflow 对账，已撤销本次错误写入：{}",
                        issues.join("；")
                    )));
                }
                apply_track_transition_defaults(&state.pool, request.project_id, script_id).await?;
                if let Some(guard) = panel_batch_guard.as_mut() {
                    guard.disarm();
                }
                let track_ids: Vec<i64> = sqlx::query_scalar(
                    "SELECT DISTINCT track_id FROM toonflow.storyboards WHERE id=ANY($1) AND track_id IS NOT NULL ORDER BY track_id",
                )
                .bind(new_rows.iter().map(|row| row.0).collect::<Vec<_>>())
                .fetch_all(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to load new video tracks"))?;
                let video_setting: Option<(Option<i64>, String)> =
                    sqlx::query_as("SELECT video_model,mode FROM toonflow.projects WHERE id=$1")
                        .bind(request.project_id)
                        .fetch_optional(&state.pool)
                        .await
                        .map_err(|_| AppError::internal("failed to load video prompt settings"))?;
                if let Some((Some(model), mode)) = video_setting.filter(|_| !track_ids.is_empty()) {
                    sqlx::query(
                        "UPDATE toonflow.video_tracks SET state='生成中',reason=NULL WHERE id=ANY($1)",
                    )
                    .bind(&track_ids)
                    .execute(&state.pool)
                    .await
                    .map_err(|_| AppError::internal("failed to start video prompts"))?;
                    let pool = state.pool.clone();
                    let project_id = request.project_id;
                    tokio::spawn(async move {
                        for track_id in track_ids {
                            let _ = crate::toonflow_video::create_prompt(
                                &pool,
                                track_id,
                                project_id,
                                &model.to_string(),
                                &mode,
                            )
                            .await;
                        }
                    });
                    output.push_str(
                        "\n\n视频提示词已开始按轨道自动生成，完成后会直接回填到每个轨道。",
                    );
                }
            }
            if agent_key == "productionAgent:deriveAssetsAgent" {
                let script_id = request
                    .script_id
                    .ok_or_else(|| AppError::bad_request("衍生资产分析缺少 scriptId"))?;
                let missing =
                    missing_appearance_derivatives(&state.pool, request.project_id, script_id)
                        .await?;
                if !missing.is_empty() {
                    let repair_prompt = format!(
                        "上轮未完整引用资产提取阶段的造型。以下 appearance 尚无衍生人物：{}。立即重新读取 assets，逐项原样复制 appearance.costumePrompt，并携带对应 appearanceId 调用 add_deriveAsset；禁止临时改写服装。",
                        missing.join("、")
                    );
                    output = Box::pin(toonflow_agents::run_scoped_production_agent_with_emitter(
                        state,
                        agent_key,
                        &sub_system,
                        &repair_prompt,
                        request.project_id,
                        request.script_id,
                        allowed_tools,
                        request.emitter.as_ref(),
                    ))
                    .await?;
                    let still_missing =
                        missing_appearance_derivatives(&state.pool, request.project_id, script_id)
                            .await?;
                    if !still_missing.is_empty() {
                        return Err(AppError::bad_request(format!(
                            "衍生资产分析未覆盖全部出场人物，仍缺少：{}",
                            still_missing.join("、")
                        )));
                    }
                }
            }
            if agent_key == "productionAgent:storyboardTableAgent" {
                let script_id = request
                    .script_id
                    .ok_or_else(|| AppError::bad_request("分镜表生成缺少 scriptId"))?;
                let (_, assets) = crate::toonflow_asset_context::load_script_context(
                    &state.pool,
                    request.project_id,
                    script_id,
                )
                .await
                .map_err(|_| AppError::internal("failed to load storyboard validation assets"))?;
                let names = crate::toonflow_storyboard_table_validation::asset_names(&assets);
                for repair_round in 0..=2 {
                    let stored: Option<Value> = sqlx::query_scalar(
                        "SELECT data->'storyboardTable' FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id=$2 AND key='productionAgent'",
                    )
                    .bind(request.project_id)
                    .bind(script_id)
                    .fetch_optional(&state.pool)
                    .await
                    .map_err(|_| AppError::internal("failed to load saved storyboard table"))?
                    .flatten();
                    let stored_content =
                        stored.as_ref().and_then(Value::as_str).and_then(|value| {
                            tagged(value, "storyboardTable").or_else(|| {
                                (!value.trim().is_empty()).then(|| value.trim().to_string())
                            })
                        });
                    let content = tagged(&output, "storyboardTable")
                        .or(stored_content)
                        .ok_or_else(|| {
                            AppError::bad_request(
                                "分镜表 Agent 既未保存 storyboardTable，也未输出完整标签",
                            )
                        })?;
                    let issues =
                        crate::toonflow_storyboard_table_validation::validate(&content, &names);
                    if issues.is_empty() {
                        break;
                    }
                    if repair_round == 2 {
                        return Err(AppError::bad_request(format!(
                            "分镜表连续修复后仍未通过写入门禁：{}",
                            issues.join("；")
                        )));
                    }
                    let repair_prompt = format!(
                        "上一版分镜表未通过写入门禁，禁止询问用户。严格重新执行 Toonflow 分镜规划流程：重新读取 script、assets、scriptPlan，先给出修复草案（估时、拆片段、桥梁元素、拆镜点、全员视觉落点），再一次性输出一份完整修正版 <storyboardTable>。必须逐项修复：\n- {}\n台词原文不得改写；机械问题全部修复后才能输出。",
                        issues.join("\n- ")
                    );
                    output = Box::pin(toonflow_agents::run_scoped_production_agent_with_emitter(
                        state,
                        agent_key,
                        &sub_system,
                        &repair_prompt,
                        request.project_id,
                        request.script_id,
                        allowed_tools,
                        request.emitter.as_ref(),
                    ))
                    .await?;
                }
            }
            if let (Some((tag, key)), Some(script_id)) = (flow_tag, request.script_id)
                && let Some(content) = tagged(&output, tag)
            {
                let mut data:Value=sqlx::query_scalar("SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id=$2 AND key='productionAgent'").bind(request.project_id).bind(script_id).fetch_optional(&state.pool).await.map_err(|_|AppError::internal("failed to load production workspace"))?.unwrap_or_else(||json!({}));
                data[key] = json!(&content);
                persist_work_data_with_transition_sync(
                    &state.pool,
                    request.project_id,
                    script_id,
                    &data,
                    now_ms(),
                )
                .await?;
            }
            let memory_role = if agent_key.ends_with(":supervisionAgent") {
                "assistant:supervision".to_string()
            } else {
                format!(
                    "assistant:execution:{}",
                    agent_key.rsplit(':').next().unwrap_or(agent_key)
                )
            };
            toonflow_agents::add_memory(
                state,
                &request.agent_type,
                &request.isolation_key,
                &memory_role,
                &toonflow_agent_runtime::strip_xml_tags(&output),
            )
            .await?;
            if agent_key == "productionAgent:storyboardTableAgent" {
                let supervision_key = "productionAgent:supervisionAgent";
                let supervision_skill =
                    toonflow_agent_runtime::load_agent_skill(&state.pool, supervision_key)
                        .await
                        .map_err(AppError::bad_request)?;
                let supervision_system = format!(
                    "{supervision_skill}\n\n你是 Toonflow 的监制。分镜表刚刚完成生成或修复并已写入工作区。必须重新读取当前 storyboardTable、script、assets，给出完整审核报告和新的 A/B/C/D 评分；不得沿用上一次评分。{project_hint}"
                );
                let audit = Box::pin(toonflow_agents::run_scoped_production_agent_with_emitter(
                    state,
                    supervision_key,
                    &supervision_system,
                    "请立即复审当前最新分镜表，输出完整问题清单和新的评分。",
                    request.project_id,
                    request.script_id,
                    &["get_flowData"],
                    request.emitter.as_ref(),
                ))
                .await?;
                toonflow_agents::add_memory(
                    state,
                    &request.agent_type,
                    &request.isolation_key,
                    "assistant:supervision",
                    &toonflow_agent_runtime::strip_xml_tags(&audit),
                )
                .await?;
                let reviewed_content = format!(
                    "{output}\n\n---\n\n## 自动复审结果（基于修复后的最新分镜表）\n{audit}"
                );
                return Ok(json!({
                    "agent": agent_key,
                    "content": reviewed_content,
                    "automaticReview": true,
                    "review": audit
                }));
            }
            Ok(json!({"agent":agent_key,"content":output}))
        }
        ("productionAgent", "set_flowData") => {
            let script_id = request
                .script_id
                .ok_or_else(|| AppError::bad_request("生产工具缺少 scriptId"))?;
            let key = request
                .arguments
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| AppError::bad_request("缺少 key"))?;
            if !matches!(
                key,
                "scriptPlan" | "storyboardTable" | "script" | "assets" | "storyboard"
            ) {
                return Err(AppError::bad_request("不支持的 Flow key"));
            }
            let value = request
                .arguments
                .get("value")
                .cloned()
                .unwrap_or(Value::Null);
            let mut data:Value=sqlx::query_scalar("SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id=$2 AND key='productionAgent'").bind(request.project_id).bind(script_id).fetch_optional(&state.pool).await.map_err(|_|AppError::internal("failed to load flow data"))?.unwrap_or_else(||json!({}));
            data[key] = value;
            persist_work_data_with_transition_sync(
                &state.pool,
                request.project_id,
                script_id,
                &data,
                now_ms(),
            )
            .await?;
            Ok(json!({"key":key,"data":data[key]}))
        }
        ("productionAgent", "add_flowData_storyboard") => {
            let script_id = request
                .script_id
                .ok_or_else(|| AppError::bad_request("生产工具缺少 scriptId"))?;
            let id = now_ms() * 1000;
            let duration = request
                .arguments
                .get("duration")
                .and_then(Value::as_i64)
                .unwrap_or(4)
                .clamp(1, 60);
            let should = request
                .arguments
                .get("shouldGenerateImage")
                .map(|value| {
                    value
                        .as_bool()
                        .unwrap_or_else(|| value.as_str() != Some("false"))
                })
                .unwrap_or(true);
            let associated_asset_ids = request
                .arguments
                .get("associateAssetsIds")
                .and_then(Value::as_array)
                .map(|ids| ids.iter().filter_map(Value::as_i64).collect::<Vec<_>>())
                .unwrap_or_default();
            let prompt = request
                .arguments
                .get("prompt")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let track = request
                .arguments
                .get("track")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("main")
                .to_string();
            let scene_key = normalize_persisted_scene_key(
                request.arguments.get("sceneKey").and_then(Value::as_str),
            )?
            .ok_or_else(|| AppError::bad_request("缺少 sceneKey，必须使用 scN 格式"))?;
            let scene_state_key = request
                .arguments
                .get("sceneStateKey")
                .and_then(Value::as_str);
            let scene_state_description = request
                .arguments
                .get("sceneStateDescription")
                .and_then(Value::as_str);
            let scene_state_parent_key = request
                .arguments
                .get("sceneStateParentKey")
                .and_then(Value::as_str);
            crate::toonflow_storyboard_asset_validation::validate_storyboard_asset_ids(
                &state.pool,
                request.project_id,
                &associated_asset_ids,
            )
            .await?;
            if should {
                let prompt_assets = crate::toonflow_asset_context::load_storyboard_prompt_assets(
                    &state.pool,
                    request.project_id,
                    &associated_asset_ids,
                )
                .await?;
                crate::toonflow_storyboard_prompt_validation::validate_storyboard_prompt(
                    prompt,
                    &prompt_assets,
                )
                .map_err(AppError::bad_request)?;
            }
            let index:i32=sqlx::query_scalar("SELECT coalesce(max(index),-1)+1 FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2").bind(request.project_id).bind(script_id).fetch_one(&state.pool).await.unwrap_or(0);
            let track_id = id + 1;
            let mut tx = state
                .pool
                .begin()
                .await
                .map_err(|_| AppError::internal("failed to add storyboard"))?;
            let track_id: i64 = if let Some(existing_track_id) = sqlx::query_scalar(
                "SELECT track_id FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND track=$3 AND track_id IS NOT NULL ORDER BY index,id LIMIT 1",
            )
            .bind(request.project_id)
            .bind(script_id)
            .bind(&track)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to find storyboard track"))?
            {
                existing_track_id
            } else {
                sqlx::query("INSERT INTO toonflow.video_tracks(id,project_id,script_id,state,duration,sort_order)VALUES($1,$2,$3,'未生成',$4,coalesce((SELECT max(sort_order)+1 FROM toonflow.video_tracks WHERE project_id=$2 AND script_id=$3),0))")
                    .bind(track_id)
                    .bind(request.project_id)
                    .bind(script_id)
                    .bind(duration as i32)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| AppError::internal("failed to add storyboard track"))?;
                track_id
            };
            let scene_state_id = crate::toonflow_scene_consistency::resolve_storyboard_scene_state(
                &mut *tx,
                request.project_id,
                script_id,
                Some(&scene_key),
                None,
                scene_state_key,
                scene_state_parent_key,
                scene_state_description,
                &associated_asset_ids,
            )
            .await?;
            sqlx::query("INSERT INTO toonflow.storyboards(id,script_id,prompt,duration,state,track_id,track,video_desc,scene_key,scene_state_id,should_generate_image,project_id,index,create_time)VALUES($1,$2,$3,$4,'未生成',$5,$6,$7,$8,$9,$10,$11,$12,$13)")
                .bind(id)
                .bind(script_id)
                .bind(prompt)
                .bind(duration.to_string())
                .bind(track_id)
                .bind(&track)
                .bind(request.arguments.get("videoDesc").and_then(Value::as_str).unwrap_or_default())
                .bind(scene_key)
                .bind(scene_state_id)
                .bind(if should { 1 } else { 0 })
                .bind(request.project_id)
                .bind(index)
                .bind(now_ms())
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to add storyboard"))?;
            sqlx::query("UPDATE toonflow.video_tracks SET duration=(SELECT coalesce(sum(CASE WHEN duration ~ '^[0-9]+$' THEN duration::integer ELSE 0 END),0)::integer FROM toonflow.storyboards WHERE track_id=$1) WHERE id=$1")
                .bind(track_id)
                .execute(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to update storyboard track duration"))?;
            if !associated_asset_ids.is_empty() {
                for (sort_order, asset_id) in associated_asset_ids.into_iter().enumerate() {
                    sqlx::query("INSERT INTO toonflow.assets_storyboards(storyboard_id,asset_id,sort_order)VALUES($1,$2,$3) ON CONFLICT(storyboard_id,asset_id) DO UPDATE SET sort_order=excluded.sort_order").bind(id).bind(asset_id).bind(sort_order as i32).execute(&mut *tx).await.map_err(|_|AppError::internal("failed to bind storyboard asset"))?;
                }
            }
            tx.commit()
                .await
                .map_err(|_| AppError::internal("failed to add storyboard"))?;
            record_storyboard_panel_created(id);
            apply_storyboard_transition_defaults_if_ready(
                &state.pool,
                request.project_id,
                script_id,
            )
            .await?;
            Ok(json!({"id":id,"index":index}))
        }
        ("productionAgent", "update_storyboard") => {
            let id = request
                .arguments
                .get("id")
                .and_then(Value::as_i64)
                .ok_or_else(|| AppError::bad_request("缺少 id"))?;
            let current = sqlx::query_as::<_, StoryboardAgentEditRow>(
                "SELECT prompt,should_generate_image,script_id,track_id,track,scene_key,scene_state_id FROM toonflow.storyboards WHERE id=$1 AND project_id=$2",
            )
            .bind(id)
            .bind(request.project_id)
            .fetch_optional(&state.pool)
                .await
                .map_err(|_| AppError::internal("failed to load storyboard"))?;
            let current = current.ok_or_else(|| AppError::not_found("storyboard not found"))?;
            let current_prompt = current.prompt;
            let current_should_generate = current.should_generate_image;
            let script_id = current.script_id;
            let current_track_id = current.track_id;
            let current_track = current.track;
            let current_scene_key = current.scene_key;
            let current_scene_state_id = current.scene_state_id;
            let prompt = request.arguments.get("prompt").and_then(Value::as_str);
            let associated_asset_ids = request
                .arguments
                .get("associateAssetsIds")
                .and_then(Value::as_array)
                .map(|ids| ids.iter().filter_map(Value::as_i64).collect::<Vec<_>>());
            if prompt.is_some() || associated_asset_ids.is_some() {
                let asset_ids = if let Some(ids) = associated_asset_ids.as_ref() {
                    crate::toonflow_storyboard_asset_validation::validate_storyboard_asset_ids(
                        &state.pool,
                        request.project_id,
                        ids,
                    )
                    .await?;
                    ids.clone()
                } else {
                    sqlx::query_scalar("SELECT asset_id FROM toonflow.assets_storyboards WHERE storyboard_id=$1 ORDER BY sort_order,asset_id").bind(id).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to load storyboard assets"))?
                };
                let should_generate = request
                    .arguments
                    .get("shouldGenerateImage")
                    .and_then(Value::as_bool)
                    .unwrap_or(current_should_generate != 0);
                if should_generate {
                    let prompt_assets =
                        crate::toonflow_asset_context::load_storyboard_prompt_assets(
                            &state.pool,
                            request.project_id,
                            &asset_ids,
                        )
                        .await?;
                    crate::toonflow_storyboard_prompt_validation::validate_storyboard_prompt(
                        prompt.unwrap_or(&current_prompt),
                        &prompt_assets,
                    )
                    .map_err(AppError::bad_request)?;
                }
            }
            let mut tx = state
                .pool
                .begin()
                .await
                .map_err(|_| AppError::internal("failed to begin storyboard update"))?;
            let target_track = request
                .arguments
                .get("track")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(current_track.as_deref().unwrap_or("main"))
                .to_string();
            let target_track_id = if (request.arguments.get("track").is_none()
                || current_track.as_deref() == Some(target_track.as_str()))
                && current_track_id.is_some()
            {
                current_track_id
            } else if let Some(existing_track_id) = sqlx::query_scalar(
                "SELECT track_id FROM toonflow.storyboards WHERE project_id=$1 AND script_id=$2 AND track=$3 AND id<>$4 AND track_id IS NOT NULL ORDER BY index,id LIMIT 1",
            )
            .bind(request.project_id)
            .bind(script_id)
            .bind(&target_track)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to find target storyboard track"))?
            {
                Some(existing_track_id)
            } else {
                let new_track_id = now_ms() * 1000 + 1;
                sqlx::query("INSERT INTO toonflow.video_tracks(id,project_id,script_id,state,duration,sort_order) VALUES($1,$2,$3,'未生成',0,coalesce((SELECT max(sort_order)+1 FROM toonflow.video_tracks WHERE project_id=$2 AND script_id=$3),0))")
                    .bind(new_track_id)
                    .bind(request.project_id)
                    .bind(script_id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| AppError::internal("failed to create target storyboard track"))?;
                Some(new_track_id)
            };
            let scene_key_was_provided = request.arguments.get("sceneKey").is_some();
            let scene_key = normalize_persisted_scene_key(
                request.arguments.get("sceneKey").and_then(Value::as_str),
            )?;
            let effective_scene_key = if scene_key_was_provided {
                scene_key.as_deref()
            } else {
                current_scene_key.as_deref()
            };
            let scene_changed = scene_key_was_provided && scene_key != current_scene_key;
            let scene_state_key = request
                .arguments
                .get("sceneStateKey")
                .and_then(Value::as_str);
            let scene_state_id = if scene_changed || scene_state_key.is_some() {
                let asset_ids = if let Some(asset_ids) = associated_asset_ids.as_ref() {
                    asset_ids.clone()
                } else {
                    sqlx::query_scalar("SELECT asset_id FROM toonflow.assets_storyboards WHERE storyboard_id=$1 ORDER BY sort_order,asset_id").bind(id).fetch_all(&mut *tx).await.map_err(|_|AppError::internal("failed to load storyboard assets"))?
                };
                crate::toonflow_scene_consistency::resolve_storyboard_scene_state(
                    &mut *tx,
                    request.project_id,
                    script_id,
                    effective_scene_key,
                    None,
                    scene_state_key,
                    request
                        .arguments
                        .get("sceneStateParentKey")
                        .and_then(Value::as_str),
                    request
                        .arguments
                        .get("sceneStateDescription")
                        .and_then(Value::as_str),
                    &asset_ids,
                )
                .await?
            } else {
                current_scene_state_id
            };
            let image_inputs_changed = prompt.is_some()
                || request.arguments.get("videoDesc").is_some()
                || associated_asset_ids.is_some()
                || scene_changed
                || scene_state_key.is_some();
            let result = sqlx::query(
                r#"UPDATE toonflow.storyboards
                   SET prompt=coalesce($3,prompt),video_desc=coalesce($4,video_desc),
                       duration=coalesce($5,duration),track=$6,track_id=$7,
                       should_generate_image=coalesce($8,should_generate_image),
                       scene_key=CASE WHEN $9 THEN $10 ELSE scene_key END,scene_state_id=$11,
                       state=CASE WHEN $12 THEN '未生成' ELSE state END,
                       reason=CASE WHEN $12 THEN '分镜描述或参考资产已更新，请重新生成图片' ELSE reason END,
                       generated_scene_state_id=CASE WHEN $12 THEN NULL ELSE generated_scene_state_id END,
                       scene_generation_context=CASE WHEN $12 THEN '{}'::jsonb ELSE scene_generation_context END
                   WHERE id=$1 AND project_id=$2"#,
            )
            .bind(id)
            .bind(request.project_id)
            .bind(prompt)
            .bind(
                request
                    .arguments
                    .get("videoDesc")
                    .and_then(Value::as_str),
            )
            .bind(
                request
                    .arguments
                    .get("duration")
                    .and_then(Value::as_i64)
                    .map(|value| value.to_string()),
            )
            .bind(&target_track)
            .bind(target_track_id)
            .bind(
                request
                    .arguments
                    .get("shouldGenerateImage")
                    .and_then(Value::as_bool)
                    .map(|value| if value { 1 } else { 0 }),
            )
            .bind(scene_key_was_provided)
            .bind(scene_key)
            .bind(scene_state_id)
            .bind(image_inputs_changed)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to update storyboard"))?;
            if result.rows_affected() == 0 {
                return Err(AppError::not_found("storyboard not found"));
            }
            if let Some(asset_ids) = associated_asset_ids {
                sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE storyboard_id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| AppError::internal("failed to reset storyboard assets"))?;
                for (sort_order, asset_id) in asset_ids.into_iter().enumerate() {
                    sqlx::query("INSERT INTO toonflow.assets_storyboards(storyboard_id,asset_id,sort_order)VALUES($1,$2,$3)").bind(id).bind(asset_id).bind(sort_order as i32).execute(&mut *tx).await.map_err(|_|AppError::internal("failed to bind storyboard asset"))?;
                }
            }
            let mut affected_track_ids = vec![current_track_id, target_track_id]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>();
            affected_track_ids.sort_unstable();
            affected_track_ids.dedup();
            for affected_track_id in affected_track_ids {
                let storyboard_count: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM toonflow.storyboards WHERE track_id=$1",
                )
                .bind(affected_track_id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| AppError::internal("failed to inspect storyboard track"))?;
                if storyboard_count == 0 {
                    sqlx::query("DELETE FROM toonflow.video_tracks WHERE id=$1")
                        .bind(affected_track_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|_| {
                            AppError::internal("failed to remove empty storyboard track")
                        })?;
                } else {
                    sqlx::query("UPDATE toonflow.video_tracks SET duration=(SELECT coalesce(sum(CASE WHEN duration ~ '^[0-9]+$' THEN duration::integer ELSE 0 END),0)::integer FROM toonflow.storyboards WHERE track_id=$1) WHERE id=$1")
                        .bind(affected_track_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|_| AppError::internal("failed to update storyboard track duration"))?;
                    if image_inputs_changed {
                        sqlx::query(
                            "UPDATE toonflow.video_tracks SET state='未生成',reason='分镜已更新，请重新生成视频' WHERE id=$1",
                        )
                        .bind(affected_track_id)
                        .execute(&mut *tx)
                        .await
                        .map_err(|_| AppError::internal("failed to invalidate storyboard track"))?;
                    }
                }
            }
            tx.commit()
                .await
                .map_err(|_| AppError::internal("failed to commit storyboard update"))?;
            record_storyboard_panel_touched(id);
            apply_storyboard_transition_defaults_if_ready(
                &state.pool,
                request.project_id,
                script_id,
            )
            .await?;
            Ok(json!(true))
        }
        ("productionAgent", "generate_storyboard") => {
            let script_id = request
                .script_id
                .ok_or_else(|| AppError::bad_request("生产工具缺少 scriptId"))?;
            let ids = request
                .arguments
                .get("ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_i64())
                .collect::<Vec<_>>();
            let rows = toonflow_image_workflow::schedule_storyboard_generation(
                &state.pool,
                request.project_id,
                script_id,
                &ids,
                request
                    .arguments
                    .get("concurrentCount")
                    .and_then(Value::as_u64)
                    .unwrap_or(2) as usize,
                false,
            )
            .await?;
            Ok(json!(rows))
        }
        ("productionAgent", "delete_storyboard") => {
            let script_id = request
                .script_id
                .ok_or_else(|| AppError::bad_request("生产工具缺少 scriptId"))?;
            let mut ids = request
                .arguments
                .get("ids")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|value| value.as_i64())
                .collect::<Vec<_>>();
            ids.sort_unstable();
            ids.dedup();
            if ids.is_empty() {
                return Err(AppError::bad_request("缺少要删除的分镜 id"));
            }
            let mut transaction = state
                .pool
                .begin()
                .await
                .map_err(|_| AppError::internal("failed to begin storyboard deletion"))?;
            let affected_rows: Vec<(i64, Option<i64>, Option<i64>)> = sqlx::query_as(
                "SELECT id,track_id,flow_id FROM toonflow.storyboards
                 WHERE id=ANY($1) AND project_id=$2 AND script_id=$3
                 FOR UPDATE",
            )
            .bind(&ids)
            .bind(request.project_id)
            .bind(script_id)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|_| AppError::internal("failed to load storyboards for deletion"))?;
            if affected_rows.len() != ids.len() {
                return Err(AppError::not_found(
                    "storyboard not found in current project script",
                ));
            }
            let mut track_ids = affected_rows
                .iter()
                .filter_map(|(_, track_id, _)| *track_id)
                .collect::<Vec<_>>();
            track_ids.sort_unstable();
            track_ids.dedup();
            let flow_ids = affected_rows
                .iter()
                .filter_map(|(_, _, flow_id)| *flow_id)
                .collect::<Vec<_>>();

            sqlx::query("DELETE FROM toonflow.assets_storyboards WHERE storyboard_id=ANY($1)")
                .bind(&ids)
                .execute(&mut *transaction)
                .await
                .map_err(|_| AppError::internal("failed to delete storyboard assets"))?;
            sqlx::query(
                "DELETE FROM toonflow.storyboards
                 WHERE id=ANY($1) AND project_id=$2 AND script_id=$3",
            )
            .bind(&ids)
            .bind(request.project_id)
            .bind(script_id)
            .execute(&mut *transaction)
            .await
            .map_err(|_| AppError::internal("failed to delete storyboards"))?;
            if !flow_ids.is_empty() {
                sqlx::query("DELETE FROM toonflow.image_flows WHERE id=ANY($1)")
                    .bind(&flow_ids)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|_| AppError::internal("failed to delete storyboard image flows"))?;
            }
            for track_id in track_ids {
                let remaining: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM toonflow.storyboards WHERE track_id=$1",
                )
                .bind(track_id)
                .fetch_one(&mut *transaction)
                .await
                .map_err(|_| AppError::internal("failed to inspect storyboard track"))?;
                if remaining == 0 {
                    sqlx::query("DELETE FROM toonflow.video_tracks WHERE id=$1")
                        .bind(track_id)
                        .execute(&mut *transaction)
                        .await
                        .map_err(|_| {
                            AppError::internal("failed to delete empty storyboard track")
                        })?;
                } else {
                    sqlx::query(
                        "UPDATE toonflow.video_tracks
                         SET duration=(
                           SELECT coalesce(sum(CASE WHEN duration ~ '^[0-9]+$'
                             THEN duration::integer ELSE 0 END),0)::integer
                           FROM toonflow.storyboards WHERE track_id=$1
                         ) WHERE id=$1",
                    )
                    .bind(track_id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|_| {
                        AppError::internal("failed to update storyboard track duration")
                    })?;
                }
            }
            transaction
                .commit()
                .await
                .map_err(|_| AppError::internal("failed to commit storyboard deletion"))?;
            apply_storyboard_transition_defaults_if_ready(
                &state.pool,
                request.project_id,
                script_id,
            )
            .await?;
            Ok(json!({"ids":ids}))
        }
        _ => Err(AppError::bad_request("不支持的 Agent 工具")),
    }
}
