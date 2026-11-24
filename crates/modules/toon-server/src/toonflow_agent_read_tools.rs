use serde_json::Value;

use crate::ToonState;

pub(crate) fn format_events(events_json: &str) -> String {
    if let Ok(events) = serde_json::from_str::<Vec<Value>>(events_json) {
        events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let name = event.get("name").and_then(Value::as_str).unwrap_or("");
                let detail = event.get("detail").and_then(Value::as_str).unwrap_or("");
                format!("  {}. {}：{}", index + 1, name, detail)
            })
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        events_json.to_string()
    }
}

/// Execute a read-only tool for sub-agents
pub(crate) async fn execute_sub_tool(
    state: &ToonState,
    project_id: i64,
    name: &str,
    args: &Value,
) -> String {
    match name {
        "get_novel_events" => {
            let indexes: Vec<i32> = args
                .get("chapterIndexs")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_i64().map(|x| x as i32))
                        .collect()
                })
                .unwrap_or_default();
            let rows: Vec<(i32, String, Option<String>)> = sqlx::query_as(
                "SELECT chapter_index,chapter,event FROM toonflow.novels WHERE project_id=$1 AND chapter_index=ANY($2) ORDER BY chapter_index",
            )
            .bind(project_id).bind(&indexes)
            .fetch_all(&state.pool).await.unwrap_or_default();
            rows.into_iter()
                .map(|(i, c, e)| {
                    format!("第{i}章「{c}」\n{}", format_events(&e.unwrap_or_default()))
                })
                .collect::<Vec<_>>()
                .join("\n\n")
        }
        "get_novel_text" => {
            let index: i32 = args
                .get("chapterIndex")
                .and_then(|v| v.as_i64())
                .unwrap_or(0) as i32;
            sqlx::query_scalar(
                "SELECT chapter_data FROM toonflow.novels WHERE project_id=$1 AND chapter_index=$2",
            )
            .bind(project_id)
            .bind(index)
            .fetch_optional(&state.pool)
            .await
            .map(|value: Option<String>| value.unwrap_or_default())
            .unwrap_or_else(|_| "读取失败：无法加载原文章节，请重试".to_string())
        }
        "get_planData" => {
            let data: Option<Value> = match sqlx::query_scalar(
                "SELECT data FROM toonflow.agent_work_data WHERE project_id=$1 AND episodes_id IS NULL AND key='scriptAgent'",
            )
            .bind(project_id).fetch_optional(&state.pool).await {
                Ok(data) => data,
                Err(_) => return "读取失败：无法加载当前工作区，请重试".to_string(),
            };
            let key = args.get("key").and_then(Value::as_str).unwrap_or("");
            if key.is_empty() || key == "scriptAgent" {
                // Return all data with clear labels
                let d = data.as_ref();
                let skeleton = d
                    .and_then(|v| v.get("storySkeleton"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("（空）");
                let adaptation = d
                    .and_then(|v| v.get("adaptationStrategy"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("（空）");
                format!(
                    "可用 key: storySkeleton, adaptationStrategy\n\n=== storySkeleton ===\n{skeleton}\n\n=== adaptationStrategy ===\n{adaptation}"
                )
            } else {
                // Try exact match first, then case-insensitive
                let result = data
                    .as_ref()
                    .and_then(|d| d.get(key))
                    .and_then(|v| v.as_str().map(|s| s.to_string()));
                if let Some(result) = result {
                    return result;
                }
                // Try known key mappings
                let mapped = match key.to_lowercase().as_str() {
                    "story_skeleton" | "skeleton" => data
                        .as_ref()
                        .and_then(|d| d.get("storySkeleton"))
                        .and_then(|v| v.as_str()),
                    "adaptation_strategy" | "adaptation" => data
                        .as_ref()
                        .and_then(|d| d.get("adaptationStrategy"))
                        .and_then(|v| v.as_str()),
                    "script" => data
                        .as_ref()
                        .and_then(|d| d.get("script"))
                        .and_then(|v| v.as_str()),
                    _ => None,
                };
                mapped.map(|s| s.to_string()).unwrap_or_else(|| {
                    format!("key '{key}' 不存在。可用 key: storySkeleton, adaptationStrategy")
                })
            }
        }
        "get_script_content" => {
            let ids: Vec<i64> = args
                .get("ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
                .unwrap_or_default();
            let rows: Vec<(String, String)> = sqlx::query_as(
                "SELECT name,content FROM toonflow.scripts WHERE project_id=$1 AND id=ANY($2)",
            )
            .bind(project_id)
            .bind(&ids)
            .fetch_all(&state.pool)
            .await
            .unwrap_or_default();
            rows.into_iter()
                .map(|(n, c)| format!("<scriptItem name=\"{n}\">{c}</scriptItem>"))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => format!("未知工具: {name}"),
    }
}
