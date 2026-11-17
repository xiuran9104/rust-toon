use crate::{
    ToonState, ai_client,
    shared::require,
    toonflow_character_identity::{normalize_age_stage, normalize_role_name},
    toonflow_prompt_store,
};
use axum::{
    Json,
    body::Body,
    extract::State,
    http::{Response, header},
};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::{Cursor, Write};

const AI_REGEX_FALLBACK: &str = r#"你是一个正则表达式专家。用户会提供一段剧本文本，你需要分析其中的集/章节分隔模式，返回一个JavaScript正则表达式字符串。

要求：
1. 正则必须包含两个捕获组：第一个捕获组匹配集数/章节编号（数字或中文数字），第二个捕获组匹配该集的标题/名称（scriptName）。
2. 返回格式为 /正则表达式/g，例如：/第\s*([0-9一二三四五六七八九十百千万]+)\s*集\s*([^\n\r]*)/g
3. 只返回正则表达式字符串本身，不要有任何其他解释文字或markdown格式。
4. 如果文本中没有明显的章节分隔模式，返回空字符串。"#;

const ASSET_EXTRACTION_FALLBACK: &str = r#"从剧本中提取后续分镜和视频生成需要保持视觉一致的基础资产与人物造型。只返回一个 JSON 对象：
{"newAssets":[{"name":"","desc":"","type":"role","scriptIds":[1]}],"existingAssetRefs":[{"name":"","desc":"","type":"role","scriptIds":[1]}],"appearances":[{"roleName":"","name":"","scenes":["场1"],"costumePrompt":"","description":"","scriptId":1}]}
分类只能是 role、scene、tool、costume。role desc 必须是至少40字的稳定可视外貌，包含性别呈现、外观年龄、五官、发型发色、肤色、身高体型和气质，不得写服装、动作或关系摘要。年龄差异写入 ageStage，仅允许 child、teen、young_adult、adult、middle_aged、senior。appearances 必须逐场覆盖每个有名角色，costumePrompt 完整描述上装、下装、鞋履、配色、面料、层次和配饰。已有资产放 existingAssetRefs，新资产放 newAssets，scriptIds 必须来自输入。
帽子、制服、衣裤、裙装、鞋靴、眼镜、口罩、手套、首饰、护甲等穿戴物一律归为 costume，不得归为 tool。被明确命名、剧情强调、需要特写或需要单独生成图片的穿戴物必须创建独立 costume 资产；普通未强调服装只保存在 appearances。禁止 Markdown。"#;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    script_ids: Vec<i64>,
    project_id: i64,
    group_size: Option<usize>,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExtractedAsset {
    #[serde(default)]
    name: String,
    #[serde(default, alias = "description")]
    desc: String,
    #[serde(rename = "type")]
    #[serde(default)]
    type_: String,
    #[serde(default)]
    script_ids: Vec<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtractedAppearance {
    #[serde(default)]
    role_name: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    scenes: Vec<String>,
    #[serde(default)]
    costume_prompt: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    script_id: i64,
    #[serde(default)]
    age_stage: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExtractedResult {
    #[serde(default)]
    new_assets: Vec<ExtractedAsset>,
    #[serde(default)]
    existing_asset_refs: Vec<ExtractedAsset>,
    #[serde(default)]
    appearances: Vec<ExtractedAppearance>,
}

fn normalized_asset_type(name: &str, description: &str, extracted_type: &str) -> String {
    const WEARABLE_MARKERS: &[&str] = &[
        "帽", "服", "衣", "裤", "裙", "鞋", "靴", "眼镜", "口罩", "手套", "围巾", "领带", "耳环",
        "耳饰", "项链", "手链", "腕表", "护甲", "披风",
    ];
    if extracted_type == "tool"
        && WEARABLE_MARKERS
            .iter()
            .any(|marker| name.contains(marker) || description.contains(marker))
    {
        "costume".to_string()
    } else {
        extracted_type.to_string()
    }
}

#[cfg(test)]
mod classification_tests {
    use super::{normalized_asset_type, parse_result};

    #[test]
    fn wearable_items_are_costumes_not_tools() {
        assert_eq!(
            normalized_asset_type("黑色鸭舌帽", "棉质", "tool"),
            "costume"
        );
        assert_eq!(
            normalized_asset_type("深紫技师服", "服务制服", "tool"),
            "costume"
        );
        assert_eq!(normalized_asset_type("水果刀", "削苹果", "tool"), "tool");
    }

    #[test]
    fn missing_required_asset_fields_do_not_reject_the_whole_response() {
        let parsed = parse_result(
            r#"{
                "newAssets": [
                    {"desc":"缺少名称的异常资产","type":"tool","scriptIds":[1]},
                    {"name":"水果刀","desc":"银色水果刀","type":"tool","scriptIds":[1]}
                ],
                "appearances": [
                    {"roleName":"小明","scenes":["场1"],"description":"缺少造型名称和服装提示词"}
                ]
            }"#,
        )
        .expect("a malformed item should not reject otherwise valid extraction JSON");

        assert_eq!(parsed.new_assets.len(), 2);
        assert!(parsed.new_assets[0].name.is_empty());
        assert_eq!(parsed.new_assets[1].name, "水果刀");
        assert_eq!(parsed.appearances.len(), 1);
        assert!(parsed.appearances[0].name.is_empty());
        assert!(parsed.appearances[0].costume_prompt.is_empty());
        assert_eq!(parsed.appearances[0].script_id, 0);
    }
}

fn parse_result(output: &str) -> Result<ExtractedResult, String> {
    let trimmed = output
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str(trimmed).or_else(|_| {
        let start = trimmed.find('{').ok_or("AI 未返回 JSON 对象")?;
        let end = trimmed.rfind('}').ok_or("AI 未返回完整 JSON 对象")?;
        serde_json::from_str(&trimmed[start..=end]).map_err(|error| error.to_string())
    })
}

async fn reconnect_legacy_derivatives(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: i64,
    script_ids: &[i64],
) -> Result<(), String> {
    sqlx::query(
        r#"WITH unmatched_appearances AS (
               SELECT ca.id AS appearance_id, ca.script_id, ca.role_asset_id,
                      row_number() OVER (
                          PARTITION BY ca.script_id, ca.role_asset_id ORDER BY ca.id
                      ) AS position,
                      count(*) OVER (
                          PARTITION BY ca.script_id, ca.role_asset_id
                      ) AS item_count
               FROM toonflow.character_appearances ca
               WHERE ca.project_id=$1 AND ca.script_id=ANY($2)
                 AND NOT EXISTS (
                     SELECT 1 FROM toonflow.assets current_asset
                     WHERE current_asset.appearance_id=ca.id
                       AND current_asset.parent_asset_id=ca.role_asset_id
                 )
           ), legacy_derivatives AS (
               SELECT d.id AS derived_id, sa.script_id, d.parent_asset_id AS role_asset_id,
                      row_number() OVER (
                          PARTITION BY sa.script_id, d.parent_asset_id ORDER BY d.id
                      ) AS position,
                      count(*) OVER (
                          PARTITION BY sa.script_id, d.parent_asset_id
                      ) AS item_count
               FROM toonflow.assets d
               JOIN toonflow.script_assets sa ON sa.asset_id=d.id
               WHERE d.project_id=$1 AND sa.script_id=ANY($2)
                 AND d.parent_asset_id IS NOT NULL AND d.appearance_id IS NULL
           ), pairs AS (
               SELECT legacy.derived_id, appearance.appearance_id
               FROM legacy_derivatives legacy
               JOIN unmatched_appearances appearance
                 ON appearance.script_id=legacy.script_id
                AND appearance.role_asset_id=legacy.role_asset_id
                AND appearance.position=legacy.position
                AND appearance.item_count=legacy.item_count
           )
           UPDATE toonflow.assets derived
           SET appearance_id=pairs.appearance_id
           FROM pairs
           WHERE derived.id=pairs.derived_id AND derived.appearance_id IS NULL"#,
    )
    .bind(project_id)
    .bind(script_ids)
    .execute(&mut **tx)
    .await
    .map_err(|error| error.to_string())?;
    Ok(())
}

async fn extract_group(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_ids: &[i64],
) -> Result<(), String> {
    let scripts = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id,name,content FROM toonflow.scripts WHERE project_id=$1 AND id=ANY($2)",
    )
    .bind(project_id)
    .bind(script_ids)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?;
    if scripts.len() != script_ids.len() {
        return Err("部分剧本不存在".into());
    }
    let project: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT project_type,type,intro,art_style FROM toonflow.projects WHERE id=$1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;
    let (project_template, project_type, project_intro, art_style) =
        project.ok_or_else(|| "项目不存在".to_string())?;
    let project_context = crate::toonflow_asset_prompt::project_world_context(
        &project_template,
        &project_type,
        &project_intro,
    );
    sqlx::query("UPDATE toonflow.scripts SET extract_state=0,error_reason=NULL WHERE project_id=$1 AND id=ANY($2)")
        .bind(project_id)
        .bind(script_ids)
        .execute(pool)
        .await
        .map_err(|error| error.to_string())?;
    let existing = sqlx::query_as::<_, (String, String, String)>(
        "SELECT a.name,a.type,a.description FROM toonflow.assets a WHERE a.project_id=$1",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| error.to_string())?
    .into_iter()
    .map(|row| format!("{}({})：{}", row.0, row.1, row.2))
    .collect::<Vec<_>>()
    .join("、");
    let content = scripts
        .iter()
        .map(|row| format!("===== 剧本ID:{} {} =====\n{}", row.0, row.1, row.2))
        .collect::<Vec<_>>()
        .join("\n\n");
    let system_prompt = toonflow_prompt_store::load_for_agent(
        pool,
        "scriptAgent",
        "scriptAssetExtraction",
        ASSET_EXTRACTION_FALLBACK,
    )
    .await;
    let system_prompt = format!(
        "{system_prompt}\n\n## Rust 输出适配器（优先级最高）\n不要调用 resultTool 或其他工具。最终只输出完整 JSON 对象，字段为 newAssets、existingAssetRefs、appearances。newAssets 和 existingAssetRefs 的每一项必须包含 name、desc、type、scriptIds；appearances 的每一项必须包含 roleName、name、scenes、costumePrompt、description、scriptId、ageStage。无法补全必填字段的条目应从数组中省略，不得省略字段或输出 null。"
    );
    let system_prompt = format!(
        "{system_prompt}\n\n{}\n提取 role 资产时，将采用的人物背景写入 desc：没有明确设定则写明中国人物形象；明确外国、混血或其他背景则保留原设定。已有角色及 appearances 继承基础角色身份，不因换装重新指定人物背景。此规则不适用于 scene、tool、costume，不要为这些资产添加人物。",
        crate::toonflow_asset_prompt::CHARACTER_IDENTITY_RULE
    );
    let system_prompt = format!(
        "{system_prompt}\n\n## 项目世界观\n{project_context}\n项目画风：{art_style}\n项目世界观用于理解古今共存、人物背景和资产归属；画风只控制视觉审美。具体资产以剧本明确事实为最高优先级，不得因画风改变电脑、服装、建筑、道具等元素的年代，也不得把项目简介里的所有对象自动添加到每一集。"
    );
    let user_prompt = format!("项目世界观：{project_context}\n已有资产：{existing}\n\n{content}");
    let mut parse_error = String::new();
    let mut result = None;
    for attempt in 0..2 {
        let repair = if attempt == 0 {
            String::new()
        } else {
            format!("\n\n上一轮 JSON 解析失败：{parse_error}。请修正并只输出完整 JSON 对象。")
        };
        let output = ai_client::project_text_untracked(
            pool,
            "universalAi",
            project_id,
            &format!("{system_prompt}{repair}"),
            &user_prompt,
        )
        .await?;
        match parse_result(&output) {
            Ok(parsed) => {
                result = Some(parsed);
                break;
            }
            Err(error) => parse_error = error,
        }
    }
    let result = result.ok_or_else(|| format!("AI 资产提取结果连续两次解析失败：{parse_error}"))?;
    let allowed = ["role", "scene", "tool", "costume"];
    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    sqlx::query("DELETE FROM toonflow.script_assets WHERE script_id=ANY($1)")
        .bind(script_ids)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
    for asset in result.new_assets {
        let asset_type = normalized_asset_type(&asset.name, &asset.desc, &asset.type_);
        if asset.name.trim().is_empty() || !allowed.contains(&asset_type.as_str()) {
            continue;
        }
        let asset_name = if asset_type == "role" {
            normalize_role_name(asset.name.trim()).0
        } else {
            asset.name.trim().to_string()
        };
        let existing_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM toonflow.assets WHERE project_id=$1 AND name=$2 AND type=$3 LIMIT 1",
        )
        .bind(project_id)
        .bind(&asset_name)
        .bind(&asset_type)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        let asset_id = if let Some(id) = existing_id {
            id
        } else {
            let id: i64 = sqlx::query_scalar("INSERT INTO toonflow.assets(name,type,description,project_id,start_time) VALUES($1,$2,$3,$4,$5) RETURNING id")
                .bind(&asset_name).bind(&asset_type).bind(asset.desc.trim()).bind(project_id).bind(chrono::Utc::now().timestamp_millis())
                .fetch_one(&mut *tx).await.map_err(|error|error.to_string())?;
            id
        };
        sqlx::query("INSERT INTO toonflow.project_assets(project_id,asset_id,linked_at) VALUES($1,$2,$3) ON CONFLICT DO NOTHING")
            .bind(project_id).bind(asset_id).bind(chrono::Utc::now().timestamp_millis())
            .execute(&mut *tx).await.map_err(|error|error.to_string())?;
        for script_id in asset
            .script_ids
            .into_iter()
            .filter(|id| script_ids.contains(id))
        {
            sqlx::query("INSERT INTO toonflow.script_assets(script_id,asset_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
                .bind(script_id).bind(asset_id).execute(&mut *tx).await.map_err(|error|error.to_string())?;
        }
    }
    for reference in result.existing_asset_refs {
        let reference_type =
            normalized_asset_type(&reference.name, &reference.desc, &reference.type_);
        let reference_name = if reference_type == "role" {
            normalize_role_name(reference.name.trim()).0
        } else {
            reference.name.trim().to_string()
        };
        let asset_id: Option<i64> = sqlx::query_scalar(
            "SELECT a.id FROM toonflow.assets a WHERE a.project_id=$1 AND a.name=$2 AND a.type=$3 LIMIT 1",
        )
        .bind(project_id)
        .bind(&reference_name)
        .bind(&reference_type)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        if let Some(asset_id) = asset_id {
            for script_id in reference
                .script_ids
                .into_iter()
                .filter(|id| script_ids.contains(id))
            {
                sqlx::query("INSERT INTO toonflow.script_assets(script_id,asset_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
                    .bind(script_id).bind(asset_id).execute(&mut *tx).await.map_err(|error|error.to_string())?;
            }
        }
    }
    let mut retained_appearance_ids = Vec::new();
    for appearance in result.appearances.into_iter() {
        if !script_ids.contains(&appearance.script_id)
            || appearance.role_name.trim().is_empty()
            || appearance.name.trim().is_empty()
            || appearance.costume_prompt.trim().is_empty()
        {
            continue;
        }
        let (role_name, inferred_from_role) = normalize_role_name(appearance.role_name.trim());
        let inferred_from_appearance = normalize_role_name(appearance.name.trim()).1;
        let age_stage = normalize_age_stage(&appearance.age_stage)
            .or(inferred_from_role)
            .or(inferred_from_appearance)
            .unwrap_or("");
        let role_asset_id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM toonflow.assets WHERE project_id=$1 AND type='role' AND name=$2 AND parent_asset_id IS NULL LIMIT 1",
        )
        .bind(project_id)
        .bind(&role_name)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        let Some(role_asset_id) = role_asset_id else {
            continue;
        };
        let retained_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.character_appearances(project_id,script_id,role_asset_id,name,age_stage,scenes,costume_prompt,description,create_time,update_time) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$9) ON CONFLICT(script_id,role_asset_id,age_stage,name) DO UPDATE SET scenes=excluded.scenes,costume_prompt=excluded.costume_prompt,description=excluded.description,update_time=excluded.update_time RETURNING id")
            .bind(project_id)
            .bind(appearance.script_id)
            .bind(role_asset_id)
            .bind(appearance.name.trim())
            .bind(age_stage)
            .bind(json!(appearance.scenes))
            .bind(appearance.costume_prompt.trim())
            .bind(appearance.description.trim())
            .bind(chrono::Utc::now().timestamp_millis())
            .fetch_one(&mut *tx)
            .await
            .map_err(|error| error.to_string())?;
        retained_appearance_ids.push(retained_id);
    }
    sqlx::query(
        "DELETE FROM toonflow.character_appearances WHERE script_id=ANY($1) AND NOT(id=ANY($2))",
    )
    .bind(script_ids)
    .bind(&retained_appearance_ids)
    .execute(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;
    reconnect_legacy_derivatives(&mut tx, project_id, script_ids).await?;
    sqlx::query("UPDATE toonflow.scripts SET extract_state=1,error_reason=NULL WHERE project_id=$1 AND id=ANY($2)")
        .bind(project_id).bind(script_ids).execute(&mut *tx).await.map_err(|error|error.to_string())?;
    tx.commit().await.map_err(|error| error.to_string())?;
    Ok(())
}

pub async fn extract_assets(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ExtractRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.script_ids.is_empty() {
        return Err(AppError::bad_request("请先选择剧本"));
    }
    let task_model = ai_client::project_model_id(&state.pool, "universalAi", request.project_id)
        .await
        .map_err(AppError::bad_request)?
        .to_string();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to queue asset extraction"))?;
    let queued = sqlx::query("UPDATE toonflow.scripts SET extract_state=2,error_reason=NULL WHERE project_id=$1 AND id=ANY($2)")
        .bind(request.project_id).bind(&request.script_ids).execute(&mut *tx).await.map_err(|_|AppError::internal("failed to queue asset extraction"))?;
    if queued.rows_affected() != request.script_ids.len() as u64 {
        return Err(AppError::not_found("部分剧本不存在，请刷新后重试"));
    }
    let related_objects = json!({"scriptIds":request.script_ids}).to_string();
    let task_input = json!({"projectId":request.project_id,"scriptIds":request.script_ids,"groupSize":request.group_size.unwrap_or(5).clamp(1,20)});
    let task_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.tasks(project_id,task_class,related_objects,model,description,state,start_time,input,progress_current,progress_total) SELECT $1,'scriptAssetExtraction',$2,$3,'剧本资产提取','running',$4,$5,0,$6 FROM toonflow.projects WHERE id=$1 RETURNING id")
        .bind(request.project_id).bind(related_objects).bind(task_model).bind(chrono::Utc::now().timestamp_millis()).bind(task_input).bind(request.script_ids.len() as i32).fetch_one(&mut *tx).await.map_err(|_|AppError::internal("failed to create asset extraction task"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to queue asset extraction"))?;
    let pool = state.pool.clone();
    let project_id = request.project_id;
    let ids = request.script_ids;
    let group_size = request.group_size.unwrap_or(5).clamp(1, 20);
    tokio::spawn(async move {
        let mut failure = None;
        for (group_index, group) in ids.chunks(group_size).enumerate() {
            if let Err(reason) = extract_group(&pool, project_id, group).await {
                let _=sqlx::query("UPDATE toonflow.scripts SET extract_state=-1,error_reason=$3 WHERE project_id=$1 AND id=ANY($2)").bind(project_id).bind(group).bind(&reason).execute(&pool).await;
                if failure.is_none() {
                    failure = Some(reason);
                }
            }
            let _ = sqlx::query("UPDATE toonflow.tasks SET progress_current=$2 WHERE id=$1")
                .bind(task_id)
                .bind(((group_index + 1) * group_size).min(ids.len()) as i32)
                .execute(&pool)
                .await;
        }
        if let Some(reason) = failure {
            let _ = sqlx::query("UPDATE toonflow.tasks SET state='failed',reason=$2 WHERE id=$1")
                .bind(task_id)
                .bind(reason)
                .execute(&pool)
                .await;
        } else {
            let counts = sqlx::query_as::<_, (i64, i64)>(
                "SELECT count(DISTINCT sa.asset_id),count(DISTINCT ca.id) FROM unnest($1::bigint[]) AS scripts(script_id) LEFT JOIN toonflow.script_assets sa ON sa.script_id=scripts.script_id LEFT JOIN toonflow.character_appearances ca ON ca.script_id=scripts.script_id",
            )
            .bind(&ids)
            .fetch_one(&pool)
            .await
            .unwrap_or((0, 0));
            let description = format!(
                "剧本资产提取完成：{} 个基础资产，{} 套人物服装/形态",
                counts.0, counts.1
            );
            let related_objects =
                json!({"scriptIds":ids,"assetCount":counts.0,"appearanceCount":counts.1})
                    .to_string();
            let _ = sqlx::query("UPDATE toonflow.tasks SET state='success',description=$2,related_objects=$3,reason=NULL WHERE id=$1")
                .bind(task_id).bind(description).bind(related_objects).execute(&pool).await;
        }
    });
    Ok(Json(ApiResponse::new(
        json!({"message":"开始提取资产","taskId":task_id}),
    )))
}

#[derive(Deserialize)]
pub struct Ids {
    ids: Vec<i64>,
}

pub async fn poll(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<Ids>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "toon:project:read")?;
    let rows = sqlx::query_as::<_, (i64, Option<i32>, Option<String>, i64)>(
        "SELECT s.id,s.extract_state,s.error_reason,(SELECT count(*) FROM toonflow.character_appearances ca WHERE ca.script_id=s.id) FROM toonflow.scripts s WHERE s.id=ANY($1) AND coalesce(s.extract_state,2)<>0",
    )
    .bind(request.ids)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to poll script assets"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter()
            .map(|row| json!({"id":row.0,"extractState":row.1,"errorReason":row.2,"appearanceCount":row.3}))
            .collect(),
    )))
}

#[derive(Deserialize)]
pub struct AiRegexRequest {
    content: String,
}

pub async fn ai_regex(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<AiRegexRequest>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "toon:project:update")?;
    let prompt =
        toonflow_prompt_store::load(&state.pool, "script_ai_regex", AI_REGEX_FALLBACK).await;
    let sample = request.content.chars().take(2000).collect::<String>();
    let result = ai_client::text(&state.pool, "universalAi", &prompt, &sample)
        .await
        .map_err(AppError::bad_request)?;
    Ok(Json(ApiResponse::new(result.trim().to_string())))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishScriptPromptRequest {
    project_id: i64,
    #[serde(alias = "content", alias = "requirement")]
    prompt: String,
}

pub async fn polish_script_prompt(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<PolishScriptPromptRequest>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "toon:project:update")?;
    if request.prompt.trim().is_empty() {
        return Err(AppError::bad_request("剧本要求不能为空"));
    }
    let system = toonflow_prompt_store::load(
        &state.pool,
        "script_prompt_polish",
        "把用户的粗略要求润色为可直接交给剧本 Agent 的结构化任务提示词。明确集数、每集时长、场号、场景、人物动作、台词和输出纪律。场景标题使用“集号-场号 场景名 日/内”，动作描述以△开头。不代写剧本，只输出润色后的提示词。",
    )
    .await;
    let output = ai_client::project_text(
        &state.pool,
        "universalAi",
        request.project_id,
        &system,
        request.prompt.trim(),
    )
    .await
    .map_err(AppError::bad_request)?;
    let output = output.trim().to_string();
    if output.is_empty() {
        return Err(AppError::bad_request("AI 未返回润色后的剧本提示词"));
    }
    Ok(Json(ApiResponse::new(output)))
}

#[derive(Deserialize)]
pub struct ExportScriptRequest {
    id: Vec<i64>,
}

pub async fn export_scripts(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ExportScriptRequest>,
) -> Result<Response<Body>, AppError> {
    require(&user, "toon:project:read")?;
    if request.id.is_empty() {
        return Err(AppError::bad_request("请先选择剧本"));
    }
    let scripts: Vec<(String, String)> =
        sqlx::query_as("SELECT name,content FROM toonflow.scripts WHERE id=ANY($1) ORDER BY id")
            .bind(request.id)
            .fetch_all(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to export scripts"))?;
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (index, (name, content)) in scripts.into_iter().enumerate() {
        let safe_name = name
            .chars()
            .map(|character| {
                if "\\/:*?\"<>|".contains(character) {
                    '_'
                } else {
                    character
                }
            })
            .collect::<String>();
        archive
            .start_file(format!("{}-{}.txt", index + 1, safe_name), options)
            .map_err(|_| AppError::internal("failed to create script archive"))?;
        archive
            .write_all(content.as_bytes())
            .map_err(|_| AppError::internal("failed to write script archive"))?;
    }
    let bytes = archive
        .finish()
        .map_err(|_| AppError::internal("failed to finish script archive"))?
        .into_inner();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/zip")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=scripts.zip",
        )
        .body(Body::from(bytes))
        .map_err(|_| AppError::internal("failed to build script archive response"))
}
