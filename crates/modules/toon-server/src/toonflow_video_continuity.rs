use serde::Serialize;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

pub(crate) const FRAME_POLICY_OWN: &str = "own";
pub(crate) const FRAME_POLICY_PREVIOUS_TAIL: &str = "previous_tail";

const TRANSITION_TYPES: &[&str] = &[
    "cut",
    "continuous",
    "action_bridge",
    "empty_shot",
    "dissolve",
    "audio_bridge",
    "match_cut",
];

struct ContinuityWorkDir(PathBuf);

impl ContinuityWorkDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ContinuityWorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn continuity_source_size_limit() -> u64 {
    std::env::var("TOON_CONTINUITY_MAX_SOURCE_BYTES")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (1024 * 1024..=10 * 1024 * 1024 * 1024).contains(value))
        .unwrap_or(512 * 1024 * 1024)
}

fn continuity_ffmpeg_timeout() -> Duration {
    let seconds = std::env::var("TOON_CONTINUITY_FFMPEG_TIMEOUT_SECONDS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (10..=3_600).contains(value))
        .unwrap_or(120);
    Duration::from_secs(seconds)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct TrackTransitionSettings {
    pub transition_type: String,
    pub frame_policy: String,
    pub previous_track_id: Option<i64>,
    pub transition_source: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FrameApplication {
    pub requested_policy: String,
    pub actual_source: String,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_track_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_video_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

impl FrameApplication {
    pub(crate) fn own() -> Self {
        Self {
            requested_policy: FRAME_POLICY_OWN.into(),
            actual_source: "storyboard".into(),
            applied: false,
            previous_track_id: None,
            previous_video_id: None,
            fallback_reason: None,
        }
    }

    #[cfg(test)]
    fn unavailable(
        previous_track_id: Option<i64>,
        previous_video_id: Option<i64>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            requested_policy: FRAME_POLICY_PREVIOUS_TAIL.into(),
            actual_source: "storyboard".into(),
            applied: false,
            previous_track_id,
            previous_video_id,
            fallback_reason: Some(reason.into()),
        }
    }
}

pub(crate) fn validate_transition_type(value: &str) -> bool {
    TRANSITION_TYPES.contains(&value)
}

pub(crate) fn validate_frame_policy(value: &str) -> bool {
    matches!(value, FRAME_POLICY_OWN | FRAME_POLICY_PREVIOUS_TAIL)
}

pub(crate) fn validate_continuity_mode(mode: &str, frame_policy: &str) -> Result<(), String> {
    if mode == "text" && frame_policy == FRAME_POLICY_PREVIOUS_TAIL {
        return Err("使用上一轨尾帧需要首帧视频模式；请切换模式或改用本轨分镜".into());
    }
    Ok(())
}

pub(crate) async fn load_track_settings(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    track_id: i64,
) -> Result<TrackTransitionSettings, String> {
    sqlx::query_as::<_, (String, String, Option<i64>, String)>(
        "SELECT transition_type,frame_policy,previous_track_id,transition_source
         FROM toonflow.video_tracks
         WHERE id=$1 AND project_id=$2 AND script_id=$3",
    )
    .bind(track_id)
    .bind(project_id)
    .bind(script_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("读取视频衔接设置失败：{error}"))?
    .map(
        |(transition_type, frame_policy, previous_track_id, transition_source)| {
            TrackTransitionSettings {
                transition_type,
                frame_policy,
                previous_track_id,
                transition_source,
            }
        },
    )
    .ok_or_else(|| "视频轨道不存在或不属于当前剧本".to_string())
}

pub(crate) async fn resolve_previous_track_id(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    track_id: i64,
    requested_previous_track_id: Option<i64>,
) -> Result<Option<i64>, String> {
    if let Some(previous_track_id) = requested_previous_track_id {
        let valid: bool = sqlx::query_scalar(
            "WITH positioned AS (
               SELECT track.id,track.sort_order,
                      coalesce(min(board.index),2147483647) AS storyboard_order
               FROM toonflow.video_tracks track
               LEFT JOIN toonflow.storyboards board ON board.track_id=track.id
               WHERE track.project_id=$2 AND track.script_id=$3
               GROUP BY track.id,track.sort_order
             ), current_track AS (
               SELECT * FROM positioned WHERE id=$4
             )
             SELECT EXISTS(
               SELECT 1
               FROM positioned previous CROSS JOIN current_track current
               WHERE previous.id=$1
                 AND (previous.storyboard_order,previous.sort_order,previous.id)
                     < (current.storyboard_order,current.sort_order,current.id)
             )",
        )
        .bind(previous_track_id)
        .bind(project_id)
        .bind(script_id)
        .bind(track_id)
        .fetch_one(pool)
        .await
        .map_err(|error| format!("校验上一视频轨道失败：{error}"))?;
        return valid.then_some(previous_track_id).map(Some).ok_or_else(|| {
            "上一视频轨道不存在、不属于当前剧本或不是当前轨道之前的轨道".to_string()
        });
    }

    sqlx::query_scalar(
        "WITH positioned AS (
           SELECT track.id,track.sort_order,
                  coalesce(min(board.index),2147483647) AS storyboard_order
           FROM toonflow.video_tracks track
           LEFT JOIN toonflow.storyboards board ON board.track_id=track.id
           WHERE track.project_id=$1 AND track.script_id=$2
           GROUP BY track.id,track.sort_order
         ), current_track AS (
           SELECT * FROM positioned WHERE id=$3
         )
         SELECT previous.id
         FROM positioned previous CROSS JOIN current_track current
         WHERE (previous.storyboard_order,previous.sort_order,previous.id)
             < (current.storyboard_order,current.sort_order,current.id)
         ORDER BY previous.storyboard_order DESC,previous.sort_order DESC,previous.id DESC
         LIMIT 1",
    )
    .bind(project_id)
    .bind(script_id)
    .bind(track_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("读取上一视频轨道失败：{error}"))
}

pub(crate) async fn selected_previous_video(
    pool: &sqlx::PgPool,
    previous_track_id: i64,
    pinned_video_id: Option<i64>,
) -> Result<Option<(i64, String)>, String> {
    sqlx::query_as(
        "SELECT video.id,video.file_path
         FROM toonflow.videos video
         WHERE video.video_track_id=$1
           AND video.state='生成成功'
           AND video.generation_context->'quality'->>'state'='passed'
           AND ($2::bigint IS NULL OR video.id=$2)
           AND ($2::bigint IS NOT NULL
                OR (SELECT video_id FROM toonflow.video_tracks WHERE id=$1) IS NULL
                OR video.id=(SELECT video_id FROM toonflow.video_tracks WHERE id=$1))
           AND coalesce(video.file_path,'')<>''
         ORDER BY (
           video.id=(SELECT video_id FROM toonflow.video_tracks WHERE id=$1)
         ) DESC NULLS LAST,video.time DESC,video.id DESC
         LIMIT 1",
    )
    .bind(previous_track_id)
    .bind(pinned_video_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("读取上一视频轨道结果失败：{error}"))
}

async fn extract_or_load_last_frame(
    pool: &sqlx::PgPool,
    project_id: i64,
    previous_video_id: i64,
    source: &str,
) -> Result<String, String> {
    let cached: Option<String> = sqlx::query_scalar(
        "SELECT file_path FROM toonflow.video_continuity_frames WHERE previous_video_id=$1",
    )
    .bind(previous_video_id)
    .fetch_optional(pool)
    .await
    .map_err(|error| format!("读取尾帧缓存失败：{error}"))?;
    if let Some(cached) = cached {
        if crate::toonflow_storage::read_asset_bytes(&cached)
            .await
            .is_ok()
        {
            return Ok(cached);
        }
        let _ =
            sqlx::query("DELETE FROM toonflow.video_continuity_frames WHERE previous_video_id=$1")
                .bind(previous_video_id)
                .execute(pool)
                .await;
    }

    let work_dir = std::env::temp_dir().join(format!(
        "rust-toon/toonflow/{project_id}/continuity-{}",
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    tokio::fs::create_dir_all(&work_dir)
        .await
        .map_err(|error| format!("创建尾帧工作目录失败：{error}"))?;
    let work_dir = ContinuityWorkDir(work_dir);
    let input_path = work_dir.path().join("previous.mp4");
    let frame_path = work_dir.path().join("last-frame.png");
    let max_bytes = continuity_source_size_limit();
    if source.starts_with("/toonflow/assets/files/")
        || source.starts_with("/api/toonflow/assets/files/")
    {
        crate::toonflow_storage::copy_asset_to_file(source, &input_path, max_bytes).await?;
    } else if source.starts_with("http://") || source.starts_with("https://") {
        crate::toonflow_video_export::download_external_video(source, &input_path, max_bytes)
            .await?;
    } else {
        return Err(format!("不支持的上一轨道视频地址：{source}"));
    }

    let mut command = tokio::process::Command::new("ffmpeg");
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-sseof",
            "-0.1",
            "-i",
        ])
        .arg(&input_path)
        .args(["-frames:v", "1", "-q:v", "2"])
        .arg(&frame_path)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(continuity_ffmpeg_timeout(), command.output())
        .await
        .map_err(|_| "提取上一轨道尾帧超时，FFmpeg 已终止".to_string())?
        .map_err(|error| format!("启动尾帧提取失败：{error}"))?;
    if !output.status.success() {
        return Err(format!(
            "提取上一轨道最后一帧失败：{}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("FFmpeg 执行失败")
        ));
    }
    let frame = tokio::fs::read(&frame_path)
        .await
        .map_err(|error| format!("读取上一轨道最后一帧失败：{error}"))?;
    let reference = crate::toonflow_storage::persist_continuity_frame_with_reservation(
        pool,
        project_id,
        previous_video_id,
        frame,
    )
    .await?;
    sqlx::query(
        "INSERT INTO toonflow.video_continuity_frames(previous_video_id,project_id,file_path,create_time)
         VALUES($1,$2,$3,$4)
         ON CONFLICT(previous_video_id) DO UPDATE
         SET file_path=excluded.file_path,create_time=excluded.create_time",
    )
    .bind(previous_video_id)
    .bind(project_id)
    .bind(&reference)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(pool)
    .await
    .map_err(|error| format!("保存尾帧缓存失败：{error}"))?;
    Ok(reference)
}

#[allow(clippy::too_many_arguments)] // 轨道衔接的全部上下文
pub(crate) async fn apply_frame_policy(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    track_id: i64,
    settings: &TrackTransitionSettings,
    mut references: Value,
    pinned_video_id: Option<i64>,
    mode: &str,
) -> Result<(Value, FrameApplication), String> {
    if settings.frame_policy == FRAME_POLICY_OWN {
        return Ok((references, FrameApplication::own()));
    }
    if settings.frame_policy != FRAME_POLICY_PREVIOUS_TAIL {
        return Err("未知的首帧来源策略".into());
    }
    let previous_track_id = resolve_previous_track_id(
        pool,
        project_id,
        script_id,
        track_id,
        settings.previous_track_id,
    )
    .await?
    .ok_or("严格连续镜头缺少上一轨道；请指定前镜头或改用本轨分镜")?;
    let Some((previous_video_id, source)) =
        selected_previous_video(pool, previous_track_id, pinned_video_id).await?
    else {
        return Err(
            "上一轨道没有通过基础质检的视频；请先生成或检查前镜头，严格连续镜头不会自动降级".into(),
        );
    };
    let last_frame = extract_or_load_last_frame(pool, project_id, previous_video_id, &source)
        .await
        .map_err(|reason| format!("上一轨道尾帧不可用，已停止连续镜头生成：{reason}"))?;
    let Some(items) = references.as_array_mut() else {
        return Err("当前模型没有可替换的首帧引用".into());
    };
    replace_first_frame(items, last_frame, mode);
    Ok((
        references,
        FrameApplication {
            requested_policy: FRAME_POLICY_PREVIOUS_TAIL.into(),
            actual_source: "previous_video_tail".into(),
            applied: true,
            previous_track_id: Some(previous_track_id),
            previous_video_id: Some(previous_video_id),
            fallback_reason: None,
        },
    ))
}

fn replace_first_frame(items: &mut Vec<Value>, last_frame: String, mode: &str) {
    // Replace the first frame, preserving the intended last frame. Inserting
    // shifted the old first frame into the provider's last-frame position.
    if mode == "startFrameOptional" && items.len() == 1 {
        items.insert(0, Value::String(last_frame));
    } else if items.is_empty() {
        items.push(Value::String(last_frame));
    } else {
        items[0] = Value::String(last_frame);
    }
}

pub(crate) fn generation_context(
    settings: &TrackTransitionSettings,
    frame: &FrameApplication,
) -> Value {
    json!({
        "transitionType": settings.transition_type,
        "transitionSource": settings.transition_source,
        "frame": frame,
    })
}

/// Applies the latest structured transition decision at generation time so an
/// older cached/base prompt cannot silently override a newly edited frame
/// policy. The base prompt still owns the shot content and visual direction.
pub(crate) fn prompt_with_transition_context(
    prompt: &str,
    settings: &TrackTransitionSettings,
    frame: &FrameApplication,
) -> String {
    let frame_instruction = if frame.applied && frame.actual_source == "previous_video_tail" {
        "首张参考图是上一轨道视频的实际尾帧；从该画面自然延续动作、构图与空间关系，不要重新建立不一致的摆放。"
    } else if settings.frame_policy == FRAME_POLICY_PREVIOUS_TAIL {
        "上一轨道尾帧当前不可用，已回退到本轨分镜参考；按本轨画面起镜，不要假定收到上一镜尾帧。"
    } else {
        "使用本轨分镜参考独立起镜；不要把上一轨道尾图当作首帧，但应遵守本镜头已有的场景与布局描述。"
    };
    format!(
        "[结构化镜头衔接约束]\n转场类型：{}\n首帧策略：{}\n{}\n\n{}",
        settings.transition_type,
        frame.actual_source,
        frame_instruction,
        prompt.trim()
    )
}

pub(crate) async fn persist_generation_context(
    pool: &sqlx::PgPool,
    video_id: i64,
    settings: &TrackTransitionSettings,
    frame: &FrameApplication,
) -> Result<(), String> {
    sqlx::query("UPDATE toonflow.videos SET generation_context=generation_context || $2::jsonb WHERE id=$1 AND state='生成中'")
        .bind(video_id)
        .bind(generation_context(settings, frame))
        .execute(pool)
        .await
        .map_err(|error| format!("保存视频首帧来源失败：{error}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn continuity_replaces_first_frame_without_shifting_the_target_tail() {
        let mut frames = vec![
            serde_json::json!("old-first"),
            serde_json::json!("intended-last"),
        ];
        super::replace_first_frame(&mut frames, "previous-tail".into(), "startEndRequired");
        assert_eq!(
            frames,
            vec![
                serde_json::json!("previous-tail"),
                serde_json::json!("intended-last")
            ]
        );
        let mut tail_only = vec![serde_json::json!("target-last")];
        super::replace_first_frame(&mut tail_only, "previous-tail".into(), "startFrameOptional");
        assert_eq!(
            tail_only,
            vec![
                serde_json::json!("previous-tail"),
                serde_json::json!("target-last")
            ]
        );
        assert!(
            super::validate_continuity_mode("text", super::FRAME_POLICY_PREVIOUS_TAIL).is_err()
        );
    }
    use super::{
        FRAME_POLICY_OWN, FRAME_POLICY_PREVIOUS_TAIL, FrameApplication, TrackTransitionSettings,
        generation_context, prompt_with_transition_context, validate_frame_policy,
        validate_transition_type,
    };

    #[test]
    fn validates_separate_transition_and_frame_contracts() {
        assert!(validate_transition_type("cut"));
        assert!(validate_transition_type("continuous"));
        assert!(!validate_transition_type("auto"));
        assert!(validate_frame_policy(FRAME_POLICY_OWN));
        assert!(validate_frame_policy(FRAME_POLICY_PREVIOUS_TAIL));
        assert!(!validate_frame_policy("always"));
    }

    #[test]
    fn records_the_effective_frame_source_for_reproducibility() {
        let settings = TrackTransitionSettings {
            transition_type: "continuous".into(),
            frame_policy: FRAME_POLICY_PREVIOUS_TAIL.into(),
            previous_track_id: Some(8),
            transition_source: "director".into(),
        };
        let frame = FrameApplication {
            requested_policy: FRAME_POLICY_PREVIOUS_TAIL.into(),
            actual_source: "previous_video_tail".into(),
            applied: true,
            previous_track_id: Some(8),
            previous_video_id: Some(9),
            fallback_reason: None,
        };
        let context = generation_context(&settings, &frame);
        assert_eq!(context["transitionType"], "continuous");
        assert_eq!(context["frame"]["previousVideoId"], 9);
        assert_eq!(context["frame"]["actualSource"], "previous_video_tail");
    }

    #[test]
    fn generation_prompt_uses_the_effective_not_only_requested_frame_source() {
        let settings = TrackTransitionSettings {
            transition_type: "continuous".into(),
            frame_policy: FRAME_POLICY_PREVIOUS_TAIL.into(),
            previous_track_id: Some(8),
            transition_source: "manual".into(),
        };
        let applied = FrameApplication {
            requested_policy: FRAME_POLICY_PREVIOUS_TAIL.into(),
            actual_source: "previous_video_tail".into(),
            applied: true,
            previous_track_id: Some(8),
            previous_video_id: Some(9),
            fallback_reason: None,
        };
        let applied_prompt = prompt_with_transition_context("人物推门", &settings, &applied);
        assert!(applied_prompt.contains("实际尾帧"));
        assert!(applied_prompt.ends_with("人物推门"));

        let fallback = FrameApplication::unavailable(Some(8), None, "missing");
        let fallback_prompt = prompt_with_transition_context("人物推门", &settings, &fallback);
        assert!(fallback_prompt.contains("已回退到本轨分镜参考"));
        assert!(!fallback_prompt.contains("首张参考图是上一轨道"));
    }
}
