pub(crate) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn video_ratio(value: &str) -> &str {
    if value.trim().is_empty() {
        "16:9"
    } else {
        value
    }
}

pub(crate) fn video_mode(value: &str) -> &str {
    if value.trim().is_empty() {
        "startEndRequired"
    } else {
        value
    }
}

use rust_toon_framework_database::PgPool;
use rust_toon_framework_web::AppError;

pub(crate) async fn ensure_project(pool: &PgPool, project_id: i64) -> Result<(), AppError> {
    let exists: Option<(i64,)> = sqlx::query_as("SELECT id FROM toonflow.projects WHERE id = $1")
        .bind(project_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| AppError::internal("failed to check project"))?;
    exists
        .map(|_| ())
        .ok_or_else(|| AppError::not_found("project not found"))
}

pub(crate) async fn validate_models(
    pool: &PgPool,
    chat: Option<i64>,
    image: Option<i64>,
    video: Option<i64>,
) -> Result<(), AppError> {
    for (id, kind) in [(chat, "chat"), (image, "image"), (video, "video")] {
        if let Some(id) = id {
            let valid: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM ai.model_configs WHERE id=$1 AND type=$2 AND status=0)",
            )
            .bind(id)
            .bind(kind)
            .fetch_one(pool)
            .await
            .map_err(|_| AppError::internal("failed to validate AI model"))?;
            if !valid {
                return Err(AppError::bad_request(format!("请选择启用的{kind}模型")));
            }
        }
    }
    Ok(())
}

pub(crate) const fn default_should_generate() -> i32 {
    1
}

#[cfg(test)]
mod tests {
    use super::{video_mode, video_ratio};

    #[test]
    fn project_media_defaults_are_stable() {
        assert_eq!(video_ratio(""), "16:9");
        assert_eq!(video_ratio("9:16"), "9:16");
        assert_eq!(video_mode(""), "startEndRequired");
        assert_eq!(video_mode("text"), "text");
    }
}
