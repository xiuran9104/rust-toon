//! Durable, deterministic media checks. A pass is technical acceptance, not a
//! claim that identity, acting, or narrative continuity has been reviewed.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use axum::{Json, extract::State};
use base64::Engine as _;
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{ToonState, shared::require, toonflow_episode_renders::ensure_project_access};

pub const VIDEO_QUALITY_JOB_KIND: &str = "toon.video_quality";

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Expectations {
    duration: Option<f64>,
    aspect_ratio: Option<String>,
    audio: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct QualityPayload {
    video_id: i64,
    project_id: i64,
    file_path: String,
    expectations: Expectations,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct QualityReport {
    version: u32,
    scope: String,
    state: String,
    checked_at: i64,
    errors: Vec<String>,
    warnings: Vec<String>,
    metadata: Value,
}

impl QualityReport {
    fn new() -> Self {
        Self {
            version: 1,
            scope: "technical".into(),
            state: "passed".into(),
            checked_at: chrono::Utc::now().timestamp_millis(),
            errors: Vec::new(),
            warnings: Vec::new(),
            metadata: json!({}),
        }
    }

    fn reject(&mut self, reason: impl Into<String>) {
        self.state = "rejected".into();
        self.errors.push(reason.into());
    }
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|v| v.is_finite())
}

fn inspect_metadata(probe: &Value, expected: &Expectations) -> QualityReport {
    let mut report = QualityReport::new();
    let streams = probe["streams"].as_array().cloned().unwrap_or_default();
    let Some(video) = streams.iter().find(|s| {
        s["codec_type"] == "video" && s["disposition"]["attached_pic"].as_i64() != Some(1)
    }) else {
        report.reject("文件没有可播放的视频流");
        return report;
    };
    let width = video["width"].as_u64().unwrap_or_default();
    let height = video["height"].as_u64().unwrap_or_default();
    let duration = number(&video["duration"]).or_else(|| number(&probe["format"]["duration"]));
    let has_audio = streams.iter().any(|s| s["codec_type"] == "audio");
    report.metadata =
        json!({"width": width, "height": height, "duration": duration, "hasAudio": has_audio});
    if width == 0 || height == 0 {
        report.reject("视频尺寸无效");
    }
    if duration.is_none_or(|d| d <= 0.0) {
        report.reject("视频时长无效");
    }
    if let (Some(actual), Some(target)) = (duration, expected.duration.filter(|d| *d > 0.0)) {
        // Accommodate model/container rounding, but reject materially truncated clips.
        if (actual - target).abs() > (target * 0.15).max(0.75) {
            report.reject(format!("视频时长 {actual:.2} 秒与请求 {target:.2} 秒不符"));
        }
    }
    if let Some((w, h)) = expected
        .aspect_ratio
        .as_deref()
        .and_then(|v| v.split_once(':'))
    {
        if let (Ok(w), Ok(h)) = (w.parse::<f64>(), h.parse::<f64>()) {
            if w > 0.0 && h > 0.0 && height > 0 {
                let sar = video["sample_aspect_ratio"]
                    .as_str()
                    .and_then(|v| v.split_once(':'))
                    .and_then(|(n, d)| Some((n.parse::<f64>().ok()?, d.parse::<f64>().ok()?)))
                    .filter(|(n, d)| *n > 0.0 && *d > 0.0)
                    .map(|(n, d)| n / d)
                    .unwrap_or(1.0);
                if ((width as f64 * sar / height as f64) / (w / h) - 1.0).abs() > 0.05 {
                    report.reject(format!("视频显示比例与请求 {w}:{h} 不符"));
                }
            }
        }
    }
    if expected.audio && !has_audio {
        report.reject("请求了音频，但生成文件没有音轨");
    }
    report
}

struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn inspect_file(path: &Path, expected: &Expectations) -> Result<QualityReport, String> {
    let mut probe = tokio::process::Command::new("ffprobe");
    probe
        .args([
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
            "-protocol_whitelist",
            "file,pipe",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(30), probe.output())
        .await
        .map_err(|_| "视频信息检查超时")?
        .map_err(|e| format!("无法启动 FFprobe：{e}"))?;
    if !output.status.success() {
        let mut report = QualityReport::new();
        report.reject("FFprobe 无法读取视频文件，文件可能损坏");
        return Ok(report);
    }
    let metadata: Value =
        serde_json::from_slice(&output.stdout).map_err(|e| format!("无法解析视频信息：{e}"))?;
    let mut report = inspect_metadata(&metadata, expected);
    if !report.errors.is_empty() {
        return Ok(report);
    }
    // Decode the entire file, not just its header. -xerror rejects corrupt frames.
    // Black/frozen sequences are warnings: intentional still shots/fades are valid.
    let mut decode = tokio::process::Command::new("ffmpeg");
    decode
        .args([
            "-nostdin",
            "-nostats",
            "-hide_banner",
            "-v",
            "info",
            "-xerror",
            "-err_detect",
            "explode",
            "-protocol_whitelist",
            "file,pipe",
            "-threads",
            "2",
            "-filter_threads",
            "1",
            "-i",
        ])
        .arg(path)
        .args([
            "-map",
            "0:v:0",
            "-map",
            "0:a?",
            "-vf",
            "blackdetect=d=0.5:pix_th=0.10,freezedetect=n=-60dB:d=2",
            "-f",
            "null",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(300), decode.output())
        .await
        .map_err(|_| "视频完整解码检查超时")?
        .map_err(|e| format!("无法启动 FFmpeg：{e}"))?;
    if !output.status.success() {
        report.reject("视频或音频未通过完整解码检查");
    }
    let diagnostics = String::from_utf8_lossy(&output.stderr);
    report.warnings = diagnostics
        .lines()
        .filter_map(|line| {
            let start = line
                .find("black_start:")
                .or_else(|| line.find("lavfi.freezedetect."))?;
            Some(line[start..].chars().take(240).collect())
        })
        .take(30)
        .collect();
    Ok(report)
}

/// Persist the file and outbox atomically. Repeated requests reuse the in-flight
/// quality job; a cancelled generation cannot be revived by a late download.
pub(crate) async fn enqueue(
    pool: &sqlx::PgPool,
    video_id: i64,
    project_id: i64,
    file_path: &str,
    recheck: bool,
) -> Result<i64, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let (state, context): (String, Value) = sqlx::query_as(
        "SELECT coalesce(state,''),generation_context FROM toonflow.videos WHERE id=$1 AND project_id=$2 FOR UPDATE",
    ).bind(video_id).bind(project_id).fetch_optional(&mut *tx).await.map_err(|e| e.to_string())?
        .ok_or("视频不存在")?;
    if state == "已取消" || (!recheck && state != "生成中") {
        return Err("视频任务已结束，拒绝继续质检".into());
    }
    if let Some(task_id) = sqlx::query_scalar::<_, i64>(
        "SELECT task_id FROM toonflow.distributed_jobs WHERE kind=$1
         AND payload->>'videoId'=$2 AND state IN ('queued','running','retry') LIMIT 1",
    )
    .bind(VIDEO_QUALITY_JOB_KIND)
    .bind(video_id.to_string())
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())?
    {
        tx.commit().await.map_err(|e| e.to_string())?;
        return Ok(task_id);
    }
    if recheck && state == "生成中" {
        return Err("视频仍在生成，请等待生成结束".into());
    }
    let request = &context["request"]["payload"];
    let payload = json!(QualityPayload {
        video_id,
        project_id,
        file_path: file_path.into(),
        expectations: Expectations {
            duration: number(&request["duration"]),
            aspect_ratio: request["aspect_ratio"].as_str().map(str::to_string),
            audio: request["audio"].as_bool().unwrap_or(false),
        },
    });
    let task_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.tasks(project_id,task_class,description,state,start_time,input,progress_total)
         VALUES($1,'videoQuality','视频基础质量检查','running',(extract(epoch FROM clock_timestamp())*1000)::bigint,$2,1) RETURNING id",
    ).bind(project_id).bind(&payload).fetch_one(&mut *tx).await.map_err(|e| e.to_string())?;
    sqlx::query("INSERT INTO toonflow.distributed_jobs(task_id,kind,payload,max_attempts) VALUES($1,$2,$3,3)")
        .bind(task_id).bind(VIDEO_QUALITY_JOB_KIND).bind(payload).execute(&mut *tx).await.map_err(|e| e.to_string())?;
    sqlx::query(
        "UPDATE toonflow.videos SET file_path=$2,state='生成中',error_reason=NULL,
         generation_context=generation_context || jsonb_build_object('quality',$3::jsonb) WHERE id=$1",
    ).bind(video_id).bind(file_path).bind(json!({"version":1,"scope":"technical","state":"pending","taskId":task_id}))
        .execute(&mut *tx).await.map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(task_id)
}

pub(crate) async fn wait_for_result(pool: &sqlx::PgPool, video_id: i64) -> bool {
    // The result survives the Gateway; this wait only coordinates a live workflow.
    loop {
        match sqlx::query_scalar::<_, String>(
            "SELECT coalesce(state,'') FROM toonflow.videos WHERE id=$1",
        )
        .bind(video_id)
        .fetch_optional(pool)
        .await
        {
            Ok(Some(state)) if state == "生成成功" => return true,
            Ok(Some(state)) if state == "生成中" => {}
            _ => return false,
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Fence every result with the worker lease and the current quality task ID.
/// Cancellation/recheck/deletion must win over a late worker result.
async fn finish(
    pool: &sqlx::PgPool,
    job_id: i64,
    task_id: i64,
    lease_token: Uuid,
    payload: &QualityPayload,
    report: &QualityReport,
) -> Result<Value, String> {
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;
    let lease = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM toonflow.distributed_jobs WHERE id=$1 AND task_id=$2
         AND state='running' AND lease_token=$3 AND lease_until>now() FOR UPDATE",
    )
    .bind(job_id)
    .bind(task_id)
    .bind(lease_token)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    if lease.is_none() {
        return Err("质检任务租约已失效".into());
    }
    let mut value = serde_json::to_value(report).map_err(|e| e.to_string())?;
    value["taskId"] = json!(task_id);
    let updated = sqlx::query(
        "UPDATE toonflow.videos SET state=$4,error_reason=$5,
         generation_context=generation_context || jsonb_build_object('quality',$6::jsonb)
         WHERE id=$1 AND project_id=$2 AND file_path=$3 AND state='生成中'
           AND generation_context->'quality'->>'taskId'=$7",
    )
    .bind(payload.video_id)
    .bind(payload.project_id)
    .bind(&payload.file_path)
    .bind(if report.state == "passed" {
        "生成成功"
    } else {
        "生成失败"
    })
    .bind(if report.errors.is_empty() {
        None
    } else {
        Some(report.errors.join("；"))
    })
    .bind(&value)
    .bind(task_id.to_string())
    .execute(&mut *tx)
    .await
    .map_err(|e| e.to_string())?;
    let result =
        json!({"videoId":payload.video_id,"quality":value,"applied":updated.rows_affected()==1});
    // Commit the result and completion together so a crash cannot lose the verdict.
    sqlx::query("UPDATE toonflow.tasks SET state='success',related_objects=$2,progress_current=1,reason=NULL WHERE id=$1 AND state='running'")
        .bind(task_id).bind(result.to_string()).execute(&mut *tx).await.map_err(|e| e.to_string())?;
    sqlx::query("UPDATE toonflow.distributed_jobs SET state='succeeded',result=$2,completed_at=now(),updated_at=now(),lease_owner=NULL,lease_token=NULL,lease_until=NULL,heartbeat_at=NULL WHERE id=$1")
        .bind(job_id).bind(&result).execute(&mut *tx).await.map_err(|e| e.to_string())?;
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(result)
}

pub async fn execute_distributed_quality(
    pool: &sqlx::PgPool,
    job_id: i64,
    task_id: i64,
    lease_token: Uuid,
    payload: Value,
) -> Result<Value, String> {
    let payload: QualityPayload = serde_json::from_value(payload).map_err(|e| e.to_string())?;
    let prefix = format!("toonflow/{}/assets/", payload.project_id);
    if crate::toonflow_storage::asset_object_key(&payload.file_path)
        .is_none_or(|key| !key.starts_with(&prefix))
    {
        return Err("质检源文件不属于当前项目存储".into());
    }
    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM toonflow.videos WHERE id=$1 AND project_id=$2 AND state='生成中'
         AND file_path=$3 AND generation_context->'quality'->>'taskId'=$4)",
    ).bind(payload.video_id).bind(payload.project_id).bind(&payload.file_path).bind(task_id.to_string())
        .fetch_one(pool).await.map_err(|e| e.to_string())?;
    if !active {
        return Ok(json!({"videoId":payload.video_id,"applied":false}));
    }
    sqlx::query(
        "UPDATE toonflow.videos SET generation_context=jsonb_set(generation_context,'{quality,state}','\"running\"'::jsonb)
         WHERE id=$1 AND state='生成中' AND generation_context->'quality'->>'taskId'=$2
           AND EXISTS(SELECT 1 FROM toonflow.distributed_jobs WHERE id=$3 AND task_id=$4 AND state='running' AND lease_token=$5 AND lease_until>now())",
    ).bind(payload.video_id).bind(task_id.to_string()).bind(job_id).bind(task_id).bind(lease_token)
        .execute(pool).await.map_err(|e| e.to_string())?;
    // Reuse the worker's stale work-directory cleanup after an abrupt crash.
    let dir = std::env::temp_dir().join(format!(
        "rust-toon/toonflow/{}/exports/work-quality-{task_id}-{lease_token}",
        payload.project_id,
    ));
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| e.to_string())?;
    let dir = WorkDir(dir);
    let file = dir.0.join("source.mp4");
    crate::toonflow_storage::copy_asset_to_file(&payload.file_path, &file, 512 * 1024 * 1024)
        .await?;
    let mut report = inspect_file(&file, &payload.expectations).await?;
    append_visual_qc(pool, &mut report, &payload, &file, &dir.0, task_id).await;
    finish(pool, job_id, task_id, lease_token, &payload, &report).await
}

/// P1 视觉质检（视频侧）：技术检查通过后抽取等距帧，上传留档并对帧执行
/// 多模态质检。结论是建议性的——记录在 quality.metadata.visualQc，不改
/// 变技术判定；自动重试编排待成本策略确认后接入。
async fn append_visual_qc(
    pool: &sqlx::PgPool,
    report: &mut QualityReport,
    payload: &QualityPayload,
    source: &Path,
    dir: &Path,
    task_id: i64,
) {
    if report.state != "passed" {
        return;
    }
    let Some(duration) = number(&report.metadata["duration"]) else {
        return;
    };
    let frames = match extract_frames(source, dir, duration).await {
        Ok(frames) if !frames.is_empty() => frames,
        _ => return,
    };
    let mut stored_paths = Vec::new();
    let mut data_urls = Vec::new();
    for (index, frame) in frames.iter().enumerate() {
        let Ok(bytes) = tokio::fs::read(frame).await else {
            continue;
        };
        data_urls.push(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ));
        if let Ok(path) = crate::toonflow_storage::persist_asset_bytes_named(
            payload.project_id,
            "video-frames",
            &format!("video-{}-{task_id}-{index}", payload.video_id),
            "png",
            bytes,
        )
        .await
        {
            stored_paths.push(path);
        }
    }
    if data_urls.is_empty() {
        return;
    }
    let expectations = video_expectations_from_context(pool, payload.video_id).await;
    let verdict = crate::toonflow_visual_qc::evaluate_frames(
        pool,
        payload.project_id,
        data_urls,
        &expectations,
    )
    .await;
    report.metadata["frames"] = json!(stored_paths);
    report.metadata["visualQc"] = match verdict {
        Ok(visual) => serde_json::to_value(&visual)
            .unwrap_or_else(|_| json!({"state": "unavailable"})),
        Err(reason) => json!({"state": "unavailable", "reason": reason}),
    };
}

/// 抽取 4 帧等距内点（10%/37%/63%/90%）。个别时间点失败只跳过该帧。
async fn extract_frames(source: &Path, dir: &Path, duration: f64) -> Result<Vec<PathBuf>, String> {
    let mut frames = Vec::new();
    for (index, ratio) in [0.1_f64, 0.37, 0.63, 0.9].into_iter().enumerate() {
        let output = dir.join(format!("qc-frame-{index}.png"));
        let status = tokio::process::Command::new("ffmpeg")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-ss")
            .arg(format!("{:.3}", duration * ratio))
            .arg("-i")
            .arg(source)
            .arg("-frames:v")
            .arg("1")
            .arg("-y")
            .arg(&output)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map_err(|error| format!("启动 FFmpeg 失败：{error}"))?;
        if status.success()
            && tokio::fs::metadata(&output)
                .await
                .is_ok_and(|metadata| metadata.len() > 0)
        {
            frames.push(output);
        }
    }
    Ok(frames)
}

/// 从视频生成快照编译视频质检期望：镜头描述（含景别）汇总、去重运镜、
/// 首帧模式下的首帧参考。快照缺失时返回空串，质检仍可运行。
async fn video_expectations_from_context(pool: &sqlx::PgPool, video_id: i64) -> String {
    let request: Option<Value> = sqlx::query_scalar(
        "SELECT generation_context->'request' FROM toonflow.videos WHERE id=$1",
    )
    .bind(video_id)
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    let Some(request) = request else {
        return String::new();
    };
    let shots = request["structuredShots"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let shots_summary = shots
        .iter()
        .map(|shot| {
            let description = shot["description"].as_str().unwrap_or_default();
            match shot["shotSize"].as_str().filter(|value| !value.is_empty()) {
                Some(size) => format!("{description}（{size}）"),
                None => description.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("；");
    let mut camera_moves = shots
        .iter()
        .filter_map(|shot| shot["cameraMove"].as_str())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    camera_moves.sort();
    camera_moves.dedup();
    let first_frame_summary = request["referenceManifest"]
        .as_array()
        .and_then(|references| {
            references
                .iter()
                .find(|reference| reference["kind"] == "first_frame")
                .or_else(|| {
                    references
                        .iter()
                        .find(|reference| reference["role"] == "first_frame")
                })
        })
        .and_then(|reference| {
            reference["name"]
                .as_str()
                .or_else(|| reference["url"].as_str())
        })
        .unwrap_or_default();
    crate::toonflow_visual_qc::video_frame_expectations(
        &shots_summary,
        &camera_moves.join("、"),
        first_frame_summary,
    )
}

#[derive(Deserialize)]
pub struct InspectRequest {
    id: i64,
}

pub async fn inspect(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<InspectRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let (project_id, file_path): (i64, Option<String>) =
        sqlx::query_as("SELECT project_id,file_path FROM toonflow.videos WHERE id=$1")
            .bind(request.id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to load video"))?
            .ok_or_else(|| AppError::not_found("视频不存在"))?;
    ensure_project_access(&state.pool, &user, project_id).await?;
    let file_path = file_path
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::bad_request("视频尚未归档"))?;
    let task_id = enqueue(&state.pool, request.id, project_id, &file_path, true)
        .await
        .map_err(AppError::bad_request)?;
    Ok(Json(ApiResponse::new(json!({"taskId":task_id}))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe() -> Value {
        json!({"streams":[{"codec_type":"video","width":1920,"height":1080,"duration":"5.0"}],"format":{"duration":"5.0"}})
    }

    #[test]
    fn validates_duration_ratio_audio_and_playable_stream() {
        let expected = Expectations {
            duration: Some(5.0),
            aspect_ratio: Some("16:9".into()),
            audio: false,
        };
        assert_eq!(inspect_metadata(&probe(), &expected).state, "passed");
        let mut value = probe();
        value["streams"][0]["duration"] = json!(1);
        value["streams"][0]["width"] = json!(1080);
        let report = inspect_metadata(
            &value,
            &Expectations {
                audio: true,
                ..expected
            },
        );
        assert_eq!(report.state, "rejected");
        assert_eq!(report.errors.len(), 3);
        value["streams"][0]["disposition"] = json!({"attached_pic":1});
        assert_eq!(
            inspect_metadata(&value, &Expectations::default()).state,
            "rejected"
        );
    }

    #[test]
    fn rejects_nonfinite_duration_and_accepts_anamorphic_display_ratio() {
        let mut value = probe();
        value["streams"][0]["duration"] = json!("NaN");
        value["format"]["duration"] = json!("N/A");
        assert_eq!(
            inspect_metadata(&value, &Expectations::default()).state,
            "rejected"
        );
        value = probe();
        value["streams"][0]["width"] = json!(1440);
        value["streams"][0]["sample_aspect_ratio"] = json!("4:3");
        assert_eq!(
            inspect_metadata(
                &value,
                &Expectations {
                    aspect_ratio: Some("16:9".into()),
                    ..Default::default()
                }
            )
            .state,
            "passed"
        );
    }

    #[tokio::test]
    #[ignore = "requires FFmpeg and FFprobe"]
    async fn decodes_real_media_and_rejects_corrupt_files() {
        let dir = std::env::temp_dir().join(format!("toon-quality-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let dir = WorkDir(dir);
        let path = dir.0.join("clip.mp4");
        let status = tokio::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=160x90:r=24:d=3",
                "-c:v",
                "libx264",
                "-y",
            ])
            .arg(&path)
            .status()
            .await
            .unwrap();
        assert!(status.success());
        let report = inspect_file(
            &path,
            &Expectations {
                duration: Some(3.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(report.state, "passed");
        assert!(
            !report.warnings.is_empty(),
            "intentional black/still footage is a warning, not a rejection"
        );
        tokio::fs::write(&path, b"not a video").await.unwrap();
        assert_eq!(
            inspect_file(&path, &Expectations::default())
                .await
                .unwrap()
                .state,
            "rejected"
        );
    }

    #[tokio::test]
    #[ignore = "requires an isolated TEST_DATABASE_URL"]
    async fn durable_quality_survives_restart_and_fences_expired_or_cancelled_results() {
        use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
        let url = std::env::var("TEST_DATABASE_URL").unwrap();
        let pool = connect(&DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap())
            .await
            .unwrap();
        migrate(&pool).await.unwrap();
        let fixture_id = chrono::Utc::now().timestamp_micros();
        let project_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.projects(id,name,create_time,update_time) VALUES($1,'quality-test',0,0) RETURNING id")
            .bind(fixture_id).fetch_one(&pool).await.unwrap();
        let script_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.scripts(id,name,project_id,create_time) VALUES($2,'quality-test',$1,0) RETURNING id")
            .bind(project_id).bind(fixture_id+1).fetch_one(&pool).await.unwrap();
        let track_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.video_tracks(id,project_id,script_id) VALUES($3,$1,$2) RETURNING id")
            .bind(project_id).bind(script_id).bind(fixture_id+2).fetch_one(&pool).await.unwrap();
        let video_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.videos(project_id,script_id,video_track_id,state,time) VALUES($1,$2,$3,'生成中',0) RETURNING id")
            .bind(project_id).bind(script_id).bind(track_id).fetch_one(&pool).await.unwrap();
        let file_path =
            format!("/toonflow/assets/files/toonflow/{project_id}/assets/videos/test.mp4");
        let task_id = enqueue(&pool, video_id, project_id, &file_path, false)
            .await
            .unwrap();
        assert_eq!(
            enqueue(&pool, video_id, project_id, &file_path, false)
                .await
                .unwrap(),
            task_id
        );
        crate::repair_gateway_interrupted_state(&pool)
            .await
            .unwrap();
        let state: String = sqlx::query_scalar("SELECT state FROM toonflow.videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            state, "生成中",
            "Gateway restart must leave the worker-owned inspection alive"
        );
        let token = Uuid::new_v4();
        let job_id: i64 = sqlx::query_scalar("UPDATE toonflow.distributed_jobs SET state='running',lease_owner='quality-test',lease_token=$2,lease_until=now()+interval '5 minutes' WHERE task_id=$1 RETURNING id")
            .bind(task_id).bind(token).fetch_one(&pool).await.unwrap();
        let payload = QualityPayload {
            video_id,
            project_id,
            file_path: file_path.clone(),
            expectations: Expectations::default(),
        };
        assert!(
            finish(
                &pool,
                job_id,
                task_id,
                Uuid::new_v4(),
                &payload,
                &QualityReport::new()
            )
            .await
            .is_err()
        );
        sqlx::query("UPDATE toonflow.distributed_jobs SET lease_until=now()-interval '1 second' WHERE id=$1")
            .bind(job_id).execute(&pool).await.unwrap();
        assert!(
            finish(
                &pool,
                job_id,
                task_id,
                token,
                &payload,
                &QualityReport::new()
            )
            .await
            .is_err()
        );
        sqlx::query("UPDATE toonflow.distributed_jobs SET lease_until=now()+interval '5 minutes' WHERE id=$1")
            .bind(job_id).execute(&pool).await.unwrap();
        let result = finish(
            &pool,
            job_id,
            task_id,
            token,
            &payload,
            &QualityReport::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["applied"], true);
        let state: String = sqlx::query_scalar("SELECT state FROM toonflow.videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(state, "生成成功");
        assert_eq!(
            crate::toonflow_video_continuity::selected_previous_video(
                &pool,
                track_id,
                Some(video_id)
            )
            .await
            .unwrap(),
            Some((video_id, file_path.clone()))
        );
        assert!(
            crate::toonflow_video_continuity::selected_previous_video(
                &pool,
                track_id,
                Some(video_id + 10000)
            )
            .await
            .unwrap()
            .is_none()
        );
        let task_id = enqueue(&pool, video_id, project_id, &file_path, true)
            .await
            .unwrap();
        assert!(
            crate::toonflow_video_continuity::selected_previous_video(
                &pool,
                track_id,
                Some(video_id)
            )
            .await
            .unwrap()
            .is_none()
        );
        let job_id: i64 = sqlx::query_scalar("UPDATE toonflow.distributed_jobs SET state='running',lease_owner='quality-test',lease_token=$2,lease_until=now()+interval '5 minutes' WHERE task_id=$1 RETURNING id")
            .bind(task_id).bind(token).fetch_one(&pool).await.unwrap();
        sqlx::query("UPDATE toonflow.videos SET state='已取消' WHERE id=$1")
            .bind(video_id)
            .execute(&pool)
            .await
            .unwrap();
        let result = finish(
            &pool,
            job_id,
            task_id,
            token,
            &payload,
            &QualityReport::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["applied"], false);
        let state: String = sqlx::query_scalar("SELECT state FROM toonflow.videos WHERE id=$1")
            .bind(video_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            state, "已取消",
            "a late verdict must never revive a cancelled generation"
        );
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
