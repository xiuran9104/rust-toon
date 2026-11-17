use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};

use crate::{ToonState, ai_client, shared::require, toonflow_prompt_store};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateRequest {
    project_id: i64,
    novel_ids: Vec<i64>,
    concurrent_count: Option<usize>,
}
pub async fn generate(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<GenerateRequest>,
) -> Result<Json<ApiResponse<&'static str>>, AppError> {
    require(&user, "toon:project:update")?;
    if req.novel_ids.is_empty() {
        return Err(AppError::bad_request("没有对应章节"));
    }
    let task_model = ai_client::project_model_id(&state.pool, "universalAi", req.project_id)
        .await
        .map_err(AppError::bad_request)?
        .to_string();
    sqlx::query("UPDATE toonflow.novels SET event_state=0,event=NULL,error_reason=NULL WHERE project_id=$1 AND id=ANY($2)").bind(req.project_id).bind(&req.novel_ids).execute(&state.pool).await.map_err(|_|AppError::internal("failed to reset novel event state"))?;
    let pool = state.pool.clone();
    let project_id = req.project_id;
    let ids = req.novel_ids;
    let concurrency = req.concurrent_count.unwrap_or(5).clamp(1, 20);
    tokio::spawn(async move {
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
        let mut jobs = Vec::new();
        for id in ids {
            let pool = pool.clone();
            let task_model = task_model.clone();
            let permit = semaphore.clone().acquire_owned().await;
            jobs.push(tokio::spawn(async move {
                if permit.is_ok() {
                    process_chapter(&pool, project_id, id, &task_model).await;
                }
            }));
        }
        for job in jobs {
            let _ = job.await;
        }
    });
    Ok(Json(ApiResponse::new("生成事件成功")))
}

async fn process_chapter(pool: &PgPool, project_id: i64, id: i64, task_model: &str) {
    let chapter: Option<(String, String)> = sqlx::query_as(
        "SELECT chapter,chapter_data FROM toonflow.novels WHERE id=$1 AND project_id=$2",
    )
    .bind(id)
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some((title, content)) = chapter else {
        return;
    };
    let task_input = json!({"projectId":project_id,"novelId":id,"chapter":title});
    // 手动提取可能重复提交，用章节级 advisory lock + running 检查
    // 保证同一章节不会并发创建两个任务。
    let mut task_tx = match pool.begin().await {
        Ok(tx) => tx,
        Err(_) => return,
    };
    if sqlx::query("SELECT pg_advisory_xact_lock($1::bigint)")
        .bind(id)
        .execute(&mut *task_tx)
        .await
        .is_err()
    {
        return;
    }
    let already_running: Option<(i64,)> = sqlx::query_as(
        "SELECT id FROM toonflow.tasks WHERE task_class='novelEvent' AND related_objects=$1 AND state='running' LIMIT 1",
    )
    .bind(id.to_string())
    .fetch_optional(&mut *task_tx)
    .await
    .ok()
    .flatten();
    if already_running.is_some() {
        return;
    }
    // PostgreSQL allocates the task identifier so repeated submissions for the
    // same chapter can never collide on `tasks_pkey`.
    let task_id: i64 = match sqlx::query_scalar("INSERT INTO toonflow.tasks(project_id,task_class,related_objects,model,description,state,start_time,input,progress_current,progress_total) VALUES($1,'novelEvent',$2,$3,$4,'running',$5,$6,0,1) RETURNING id")
        .bind(project_id)
        .bind(id.to_string())
        .bind(task_model)
        .bind(format!("提取事件：{title}"))
        .bind(chrono::Utc::now().timestamp_millis())
        .bind(task_input)
        .fetch_one(&mut *task_tx)
        .await
    {
        Ok(task_id) => task_id,
        Err(_) => return,
    };
    if task_tx.commit().await.is_err() {
        return;
    }
    // The Agent binding is the default route; this key is the compatibility fallback.
    let system = toonflow_prompt_store::load_for_agent(pool, "universalAi", "eventExtraction", r#"你是小说文本分析助手。用户每次提供一个章节的原文，你提取该章的结构化事件信息。

## ⚠️ 输出约束（最高优先级，违反任何一条即为失败）
1. 只输出纯 JSON 数组，第一个字符必须是 `[`
2. 不输出任何引导语、解释、总结、Markdown、代码块标记
3. 不输出表头行、分隔线、emoji

## 输出格式
[
  {"name":"15字以内事件名","detail":"核心事件描述","characters":"涉及角色","mainline":"强/中/弱（理由）","density":"高/中/低","duration":"X秒","mood":"情绪标签"}
]

## 字段规范
- name：动作+结果，15字以内，禁止笼统（如"主角经历了一些事"）
- detail：30-60字，必须含：谁做了什么→什么结果→对主线的影响
- characters：有实际戏份的角色名，顿号分隔
- mainline：强/中/弱 + （3-8字理由）。强=直接推动主角弧线；中=补充世界观/人物关系/伏笔；弱=过渡/气氛
- density：高/中/低。高密度+高情绪→45-60秒；中→35-45秒；低→25-35秒
- duration：预估集长，格式为"X秒"，禁止用分钟
- mood：从[冲突/情感/转折/高潮/悬疑/平铺/喜剧/爽感/压抑/期待/震撼/温馨]中选

## 提取规则
- 忠于原文，不推测不脑补，不加入原文未出现的情节
- 多条平行事件线时，选对主角影响最大的，其余简要带过
- 对话密集章节，关注对话推动了什么结果，而非复述对话内容
- 每章只概括一条核心事件，以整章为单位，保留主要行动、结果和转折，不按字数或场景拆分"#).await;
    let system = format!(
        "{system}\n\n## 章节输出约束\n不要调用工具。每章只概括一条核心事件，不按字数或场景拆分。最终只输出包含一个对象的 JSON 数组，字段为 name、detail、characters、mainline、density、duration、mood。"
    );
    match ai_client::project_text_untracked(
        pool,
        "universalAi",
        project_id,
        &system,
        &format!("章节：{title}\n\n{content}"),
    )
    .await
    {
        Ok(raw) => {
            if let Err(reason) = save_events(pool, id, &raw).await {
                fail(pool, id, task_id, &reason).await
            } else {
                let _ = sqlx::query(
                    "UPDATE toonflow.tasks SET state='success',progress_current=1 WHERE id=$1",
                )
                .bind(task_id)
                .execute(pool)
                .await;
            }
        }
        Err(reason) => fail(pool, id, task_id, &reason).await,
    }
}
async fn save_events(pool: &PgPool, novel_id: i64, raw: &str) -> Result<(), String> {
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    if cleaned.is_empty() {
        return Err("事件提取模型返回了空内容，请检查模型输出上限或更换模型后重试".into());
    }
    let events: Vec<Value> =
        serde_json::from_str(cleaned).map_err(|e| format!("事件 JSON 解析失败: {e}"))?;
    if events.is_empty() {
        return Err("事件提取模型返回了空数组，请调整提示词后重试".into());
    }
    if events.len() != 1 {
        return Err(format!("每章应概括一条核心事件，模型返回了 {} 条，请重新提取", events.len()));
    }
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    // Delete the events this novel owns before writing its replacement. The
    // event identifiers come from a sequence, so they are resolved through the
    // chapter links instead of a derived identifier range.
    sqlx::query(
        "DELETE FROM toonflow.events e USING toonflow.event_chapters c
         WHERE c.event_id = e.id AND c.novel_id = $1",
    )
    .bind(novel_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM toonflow.event_chapters WHERE novel_id=$1")
        .bind(novel_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    for event in events.iter() {
        let name = event
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("未命名事件");
        let detail = event.get("detail").and_then(Value::as_str).unwrap_or("");
        let event_id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.events(name,detail,create_time) VALUES($1,$2,$3) RETURNING id",
        )
        .bind(name)
        .bind(detail)
        .bind(chrono::Utc::now().timestamp_millis())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
        sqlx::query("INSERT INTO toonflow.event_chapters(event_id,novel_id) VALUES($1,$2)")
            .bind(event_id)
            .bind(novel_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    sqlx::query("UPDATE toonflow.novels SET event=$2,event_state=1,error_reason=NULL WHERE id=$1")
        .bind(novel_id)
        .bind(raw)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}
async fn fail(pool: &PgPool, id: i64, task_id: i64, reason: &str) {
    let _ = sqlx::query("UPDATE toonflow.novels SET event_state=-1,error_reason=$2 WHERE id=$1")
        .bind(id)
        .bind(reason)
        .execute(pool)
        .await;
    let _ = sqlx::query("UPDATE toonflow.tasks SET state='failed',reason=$2 WHERE id=$1")
        .bind(task_id)
        .bind(reason)
        .execute(pool)
        .await;
}

#[derive(Deserialize)]
pub struct StatesRequest {
    ids: Vec<i64>,
}
pub async fn states(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<StatesRequest>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows=sqlx::query_as::<_,(i64,Option<String>,i32,Option<String>)>("SELECT id,event,event_state,error_reason FROM toonflow.novels WHERE id=ANY($1) AND event_state<>0").bind(req.ids).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to list event states"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter()
            .map(|r| json!({"id":r.0,"event":r.1,"eventState":r.2,"errorReason":r.3}))
            .collect(),
    )))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRequest {
    project_id: i64,
    page: i64,
    limit: i64,
    search: Option<String>,
}
#[derive(Serialize, FromRow)]
#[serde(rename_all = "camelCase")]
struct EventRow {
    id: i64,
    event_name: String,
    detail: String,
    create_time: i64,
    chapters: Vec<i32>,
}
pub async fn list(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<ListRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:episode:read")?;
    let search = req.search.filter(|s| !s.is_empty());
    let total:(i64,)=sqlx::query_as("SELECT count(DISTINCT e.id) FROM toonflow.events e JOIN toonflow.event_chapters ec ON ec.event_id=e.id JOIN toonflow.novels n ON n.id=ec.novel_id WHERE n.project_id=$1 AND ($2::text IS NULL OR e.name ILIKE '%'||$2||'%')").bind(req.project_id).bind(&search).fetch_one(&state.pool).await.map_err(|_|AppError::internal("failed to count events"))?;
    let rows=sqlx::query_as::<_,EventRow>("SELECT e.id,e.name event_name,e.detail,e.create_time,array_agg(n.chapter_index ORDER BY n.chapter_index)::int4[] chapters FROM toonflow.events e JOIN toonflow.event_chapters ec ON ec.event_id=e.id JOIN toonflow.novels n ON n.id=ec.novel_id WHERE n.project_id=$1 AND ($2::text IS NULL OR e.name ILIKE '%'||$2||'%') GROUP BY e.id ORDER BY e.id DESC OFFSET $3 LIMIT $4").bind(req.project_id).bind(search).bind((req.page.max(1)-1)*req.limit.clamp(1,100)).bind(req.limit.clamp(1,100)).fetch_all(&state.pool).await.map_err(|_|AppError::internal("failed to list events"))?;
    Ok(Json(ApiResponse::new(json!({"list":rows,"total":total.0}))))
}

#[derive(Deserialize)]
pub struct DeleteRequest {
    id: i64,
}
#[derive(Deserialize)]
pub struct BatchDeleteRequest {
    ids: Vec<i64>,
}
pub async fn delete(
    user: CurrentUser,
    state: State<ToonState>,
    Json(req): Json<DeleteRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    batch_delete(user, state, Json(BatchDeleteRequest { ids: vec![req.id] })).await
}
pub async fn batch_delete(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(req): Json<BatchDeleteRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "toon:episode:delete")?;
    sqlx::query("DELETE FROM toonflow.events WHERE id=ANY($1)")
        .bind(req.ids)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to delete events"))?;
    Ok(Json(ApiResponse::new(())))
}
