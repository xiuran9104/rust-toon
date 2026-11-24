use crate::{
    ToonState,
    shared::require,
    toonflow_episode_renders::{ensure_project_access, ensure_script_in_project},
};
use axum::{Json, extract::State};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

pub const VIDEO_EXPORT_JOB_KIND: &str = "toon.video_export";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoExportJobPayload {
    pub project_id: i64,
    pub script_id: i64,
    #[serde(default)]
    pub video_ids: Vec<i64>,
    #[serde(default)]
    pub sources: Vec<VideoExportSource>,
    pub created_by: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoExportSource {
    pub video_id: i64,
    pub file_path: String,
    /// Boundary transition from the previous source into this clip. The first
    /// source ignores it because there is no predecessor on the timeline.
    #[serde(default = "default_transition_type")]
    pub transition_type: String,
    #[serde(default = "default_transition_duration_ms")]
    pub transition_duration_ms: i64,
    #[serde(default)]
    pub trim_start_ms: i64,
    #[serde(default)]
    pub trim_end_ms: Option<i64>,
}

fn default_transition_type() -> String {
    "cut".into()
}

fn default_transition_duration_ms() -> i64 {
    600
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRequest {
    pub project_id: i64,
    pub script_id: i64,
    #[serde(default)]
    pub video_ids: Vec<i64>,
}

struct ExportArtifact {
    object_path: String,
    file_path: String,
    source_video_ids: Vec<i64>,
    metadata: Value,
}

struct ExportWorkDir(PathBuf);

impl ExportWorkDir {
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ExportWorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn env_u64(name: &str, default: u64, minimum: u64, maximum: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| (*value >= minimum) && (*value <= maximum))
        .unwrap_or(default)
}

fn source_size_limit() -> u64 {
    env_u64(
        "TOON_WORKER_MAX_SOURCE_BYTES",
        2 * 1024 * 1024 * 1024,
        1024 * 1024,
        100 * 1024 * 1024 * 1024,
    )
}

fn job_source_size_limit() -> u64 {
    env_u64(
        "TOON_WORKER_MAX_JOB_SOURCE_BYTES",
        10 * 1024 * 1024 * 1024,
        1024 * 1024,
        500 * 1024 * 1024 * 1024,
    )
}

/// Remove only old, attempt-scoped export directories left by abrupt process
/// death. Active attempts use unique fencing-token paths and the default
/// seven-day threshold is well above the maximum supported FFmpeg timeout.
pub async fn cleanup_stale_export_workdirs() -> Result<u64, String> {
    let root = std::env::temp_dir().join("rust-toon/toonflow");
    let mut projects = match tokio::fs::read_dir(&root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("扫描成片临时目录失败：{error}")),
    };
    let retention = Duration::from_secs(env_u64(
        "TOON_WORKER_STALE_WORKDIR_SECONDS",
        7 * 24 * 60 * 60,
        3_600,
        365 * 24 * 60 * 60,
    ));
    let now = std::time::SystemTime::now();
    let mut removed = 0_u64;
    while let Some(project) = projects
        .next_entry()
        .await
        .map_err(|error| format!("读取成片项目临时目录失败：{error}"))?
    {
        let file_type = project
            .file_type()
            .await
            .map_err(|error| format!("读取成片项目目录类型失败：{error}"))?;
        if !file_type.is_dir() {
            continue;
        }
        let exports = project.path().join("exports");
        let mut attempts = match tokio::fs::read_dir(exports).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(format!("扫描成片尝试目录失败：{error}")),
        };
        while let Some(attempt) = attempts
            .next_entry()
            .await
            .map_err(|error| format!("读取成片尝试目录失败：{error}"))?
        {
            let name = attempt.file_name();
            if !name.to_string_lossy().starts_with("work-")
                || !attempt
                    .file_type()
                    .await
                    .map_err(|error| format!("读取成片尝试目录类型失败：{error}"))?
                    .is_dir()
            {
                continue;
            }
            let modified = attempt
                .metadata()
                .await
                .and_then(|metadata| metadata.modified())
                .map_err(|error| format!("读取成片尝试目录时间失败：{error}"))?;
            if now.duration_since(modified).unwrap_or_default() < retention {
                continue;
            }
            tokio::fs::remove_dir_all(attempt.path())
                .await
                .map_err(|error| format!("清理过期成片临时目录失败：{error}"))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn media_timeout() -> Duration {
    Duration::from_secs(env_u64(
        "TOON_WORKER_FFMPEG_TIMEOUT_SECONDS",
        7_200,
        30,
        86_400,
    ))
}

async fn command_output(
    command: &mut tokio::process::Command,
    timeout: Duration,
    label: &str,
) -> Result<Output, String> {
    command.kill_on_drop(true);
    tokio::time::timeout(timeout, command.output())
        .await
        .map_err(|_| format!("{label}超时，子进程已终止"))?
        .map_err(|error| format!("启动{label}失败：{error}"))
}

fn object_path_from_file_path(file_path: &str) -> String {
    file_path
        .strip_prefix("/api/toonflow/assets/files/")
        .or_else(|| file_path.strip_prefix("/toonflow/assets/files/"))
        .unwrap_or(file_path)
        .to_string()
}

async fn ffmpeg_available() -> bool {
    let mut ffmpeg = tokio::process::Command::new("ffmpeg");
    ffmpeg
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut ffprobe = tokio::process::Command::new("ffprobe");
    ffprobe
        .arg("-version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command_output(&mut ffmpeg, Duration::from_secs(10), "FFmpeg 检查")
        .await
        .is_ok_and(|output| output.status.success())
        && command_output(&mut ffprobe, Duration::from_secs(10), "FFprobe 检查")
            .await
            .is_ok_and(|output| output.status.success())
}

async fn probe_dimensions(path: &Path) -> Result<(u32, u32), String> {
    let mut command = tokio::process::Command::new("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
        ])
        .arg(path)
        .stdin(Stdio::null());
    let output = command_output(&mut command, Duration::from_secs(30), "视频尺寸探测").await?;
    if !output.status.success() {
        return Err(format!(
            "探测视频尺寸失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value = String::from_utf8_lossy(&output.stdout);
    let (width, height) = value
        .trim()
        .split_once('x')
        .ok_or_else(|| "无法识别视频尺寸".to_string())?;
    Ok((
        width.parse().map_err(|_| "无法识别视频宽度".to_string())?,
        height.parse().map_err(|_| "无法识别视频高度".to_string())?,
    ))
}

async fn probe_has_audio(path: &Path) -> Result<bool, String> {
    let mut command = tokio::process::Command::new("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=index",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .stdin(Stdio::null());
    let output = command_output(&mut command, Duration::from_secs(30), "视频音频探测").await?;
    if !output.status.success() {
        return Err(format!(
            "探测视频音频失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(!output.stdout.is_empty())
}

async fn probe_duration(path: &Path) -> Result<f64, String> {
    let mut command = tokio::process::Command::new("ffprobe");
    command
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .stdin(Stdio::null());
    let output = command_output(&mut command, Duration::from_secs(30), "视频时长探测").await?;
    if !output.status.success() {
        return Err(format!(
            "探测视频时长失败：{}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let duration: f64 = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "无法识别视频时长".to_string())?;
    if !(duration > 0.0 && duration.is_finite()) {
        return Err("视频时长无效".into());
    }
    Ok(duration)
}

/// One frozen clip on the export timeline. `transition_type`/`transition_
/// duration_ms` describe the boundary from the previous clip into this one.
#[derive(Clone, Debug, PartialEq)]
struct TimelineClip {
    duration_ms: i64,
    has_audio: bool,
    trim_start_ms: i64,
    trim_end_ms: Option<i64>,
    transition_type: String,
    transition_duration_ms: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct TimelinePlan {
    filter_graph: String,
    silent_inputs: usize,
    total_duration_ms: i64,
}

/// Below this overlap a transition is indistinguishable from a hard cut and
/// only risks xfade/acrossfade rounding errors.
const MIN_TRANSITION_MS: i64 = 40;

fn seconds(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn clip_play_range(clip: &TimelineClip) -> Result<(i64, i64), String> {
    let start = clip.trim_start_ms.max(0);
    if start >= clip.duration_ms {
        return Err(format!(
            "视频裁切起点 {start}ms 已达到或超出素材时长 {}ms",
            clip.duration_ms
        ));
    }
    let end = clip
        .trim_end_ms
        .unwrap_or(clip.duration_ms)
        .min(clip.duration_ms);
    if end - start < 100 {
        return Err(format!("视频裁切区间过短（{start}ms - {end}ms）"));
    }
    Ok((start, end))
}

/// Build a single-pass filter graph covering normalization (scale/pad/fps/
/// sample-rate), trim points, and per-boundary transitions:
/// - `dissolve` crossfades video (xfade) and audio (acrossfade);
/// - `audio_bridge` keeps a hard video cut but leads the next clip's audio in
///   early (acrossfade), so later audio shifts earlier by the overlap;
/// - every other boundary type is a hard cut at edit time (the remaining
///   types steer generation, not the edit).
fn build_timeline_plan(
    clips: &[TimelineClip],
    width: u32,
    height: u32,
) -> Result<TimelinePlan, String> {
    if clips.is_empty() {
        return Err("导出时间线缺少视频片段".into());
    }
    let clip_count = clips.len();
    let mut graph = String::new();
    let mut silent_inputs = 0_usize;
    let mut play_durations = Vec::with_capacity(clip_count);
    for (index, clip) in clips.iter().enumerate() {
        let (start, end) = clip_play_range(clip)?;
        play_durations.push(end - start);
        graph.push_str(&format!(
            "[{index}:v]trim=start={}:end={},setpts=PTS-STARTPTS,scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2:color=black,fps=30,format=yuv420p,setsar=1[v{index}];",
            seconds(start),
            seconds(end)
        ));
        if clip.has_audio {
            graph.push_str(&format!(
                "[{index}:a]atrim=start={}:end={},asetpts=N/SR/TB,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo[a{index}];",
                seconds(start),
                seconds(end)
            ));
        } else {
            let silent_index = clip_count + silent_inputs;
            silent_inputs += 1;
            graph.push_str(&format!(
                "[{silent_index}:a]atrim=end={},asetpts=N/SR/TB,aresample=48000,aformat=sample_fmts=fltp:channel_layouts=stereo[a{index}];",
                seconds(end - start)
            ));
        }
    }
    let mut current_video = "v0".to_string();
    let mut current_audio = "a0".to_string();
    let mut video_ms = play_durations[0];
    for index in 1..clip_count {
        let clip = &clips[index];
        let clip_ms = play_durations[index];
        let overlap_ms = match clip.transition_type.as_str() {
            "dissolve" | "audio_bridge" => clip
                .transition_duration_ms
                .clamp(0, video_ms.min(clip_ms) * 9 / 10),
            _ => 0,
        };
        let (next_video, next_audio) = (format!("xv{index}"), format!("xa{index}"));
        match (
            clip.transition_type.as_str(),
            overlap_ms >= MIN_TRANSITION_MS,
        ) {
            ("dissolve", true) => {
                graph.push_str(&format!(
                    "[{current_video}][v{index}]xfade=transition=fade:duration={}:offset={}[{next_video}];",
                    seconds(overlap_ms),
                    seconds(video_ms - overlap_ms)
                ));
                graph.push_str(&format!(
                    "[{current_audio}][a{index}]acrossfade=d={}[{next_audio}];",
                    seconds(overlap_ms)
                ));
                video_ms += clip_ms - overlap_ms;
            }
            ("audio_bridge", true) => {
                graph.push_str(&format!(
                    "[{current_video}][v{index}]concat=n=2:v=1:a=0[{next_video}];"
                ));
                graph.push_str(&format!(
                    "[{current_audio}][a{index}]acrossfade=d={}[{next_audio}];",
                    seconds(overlap_ms)
                ));
                video_ms += clip_ms;
            }
            _ => {
                graph.push_str(&format!(
                    "[{current_video}][v{index}]concat=n=2:v=1:a=0[{next_video}];"
                ));
                graph.push_str(&format!(
                    "[{current_audio}][a{index}]concat=n=2:v=0:a=1[{next_audio}];"
                ));
                video_ms += clip_ms;
            }
        }
        current_video = next_video;
        current_audio = next_audio;
    }
    graph.push_str(&format!("[{current_video}]null[vout];"));
    // Audio bridges shift later audio earlier than its video; pad any tail so
    // the audio stream always covers the full video duration.
    graph.push_str(&format!(
        "[{current_audio}]apad,atrim=end={},aresample=48000[aout]",
        seconds(video_ms)
    ));
    Ok(TimelinePlan {
        filter_graph: graph,
        silent_inputs,
        total_duration_ms: video_ms,
    })
}

fn ipv4_is_non_public(address: Ipv4Addr) -> bool {
    let octets = address.octets();
    address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_broadcast()
        || address.is_unspecified()
        || address.is_multicast()
        || octets[0] == 0
        || octets[0] >= 224
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
        || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
        || (octets[0] == 198 && (octets[1] == 18 || octets[1] == 19))
        || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
        || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
}

fn ipv6_is_non_public(address: Ipv6Addr) -> bool {
    if let Some(mapped) = address.to_ipv4_mapped() {
        return ipv4_is_non_public(mapped);
    }
    let segments = address.segments();
    address.is_loopback()
        || address.is_unspecified()
        || address.is_multicast()
        || (segments[0] & 0xfe00) == 0xfc00
        || (segments[0] & 0xffc0) == 0xfe80
        || (segments[0] == 0x2001 && segments[1] == 0x0db8)
}

fn ip_is_non_public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => ipv4_is_non_public(address),
        IpAddr::V6(address) => ipv6_is_non_public(address),
    }
}

async fn safe_video_client(url: &reqwest::Url) -> Result<reqwest::Client, String> {
    let allow_http = std::env::var("TOON_WORKER_ALLOW_HTTP_SOURCES")
        .ok()
        .is_some_and(|value| matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true"));
    if url.scheme() != "https" && !(allow_http && url.scheme() == "http") {
        return Err("外部源视频必须使用 HTTPS".into());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("外部源视频地址不能包含用户凭据".into());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "外部源视频地址缺少主机名".to_string())?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| "外部源视频地址端口无效".to_string())?;
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|error| format!("解析源视频主机失败：{error}"))?
        .collect::<Vec<SocketAddr>>();
    if addresses.is_empty()
        || addresses
            .iter()
            .any(|address| ip_is_non_public(address.ip()))
    {
        return Err("外部源视频地址解析到非公网网络，已拒绝访问".into());
    }
    let pinned = addresses[0];
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(source_download_timeout())
        .resolve(host, pinned)
        .build()
        .map_err(|error| format!("创建安全下载客户端失败：{error}"))
}

pub(crate) fn source_download_timeout() -> Duration {
    Duration::from_secs(env_u64(
        "TOON_WORKER_SOURCE_TIMEOUT_SECONDS",
        1_800,
        10,
        86_400,
    ))
}

pub(crate) async fn download_external_video(
    source: &str,
    destination: &Path,
    max_bytes: u64,
) -> Result<u64, String> {
    let url = reqwest::Url::parse(source).map_err(|error| format!("视频地址无效：{error}"))?;
    let response = rust_toon_framework_resilience::inject_trace_context(
        safe_video_client(&url).await?.get(url),
    )
    .send()
    .await
    .map_err(|error| format!("下载视频失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "下载视频失败：HTTP {}（不跟随重定向）",
            response.status()
        ));
    }
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes)
    {
        return Err(format!("源视频超过大小限制（最大 {max_bytes} 字节）"));
    }
    let mut file = tokio::fs::File::create(destination)
        .await
        .map_err(|error| format!("创建视频临时文件失败：{error}"))?;
    let mut total = 0_u64;
    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("读取视频流失败：{error}"))?;
        total = total
            .checked_add(chunk.len() as u64)
            .ok_or_else(|| "源视频大小溢出".to_string())?;
        if total > max_bytes {
            return Err(format!("源视频超过大小限制（最大 {max_bytes} 字节）"));
        }
        file.write_all(&chunk)
            .await
            .map_err(|error| format!("保存视频失败：{error}"))?;
    }
    file.flush()
        .await
        .map_err(|error| format!("刷新视频临时文件失败：{error}"))?;
    Ok(total)
}

async fn materialize_video(
    source: &str,
    directory: &Path,
    index: usize,
) -> Result<PathBuf, String> {
    let destination = directory.join(format!("{index:04}.mp4"));
    let max_bytes = source_size_limit();
    if source.starts_with("/toonflow/assets/files/")
        || source.starts_with("/api/toonflow/assets/files/")
    {
        crate::toonflow_storage::copy_asset_to_file(source, &destination, max_bytes).await?;
    } else if source.starts_with("http://") || source.starts_with("https://") {
        download_external_video(source, &destination, max_bytes).await?;
    } else {
        return Err(format!("不支持的视频地址：{source}"));
    }
    tokio::fs::canonicalize(destination)
        .await
        .map_err(|error| format!("解析视频路径失败：{error}"))
}

async fn resolve_export_sources(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    project_id: i64,
    script_id: i64,
    requested_video_ids: &[i64],
) -> Result<Vec<VideoExportSource>, AppError> {
    // Determine the cinematic order first, then acquire every video lock in a
    // stable ID order. Project/script deletion uses that same child lock order,
    // while the final vector below restores the track/storyboard order.
    let ordered_video_ids: Vec<i64> = if requested_video_ids.is_empty() {
        sqlx::query_scalar("SELECT v.id FROM toonflow.video_tracks t JOIN toonflow.videos v ON v.id=COALESCE((SELECT selected.id FROM toonflow.videos selected WHERE selected.id=t.video_id AND selected.video_track_id=t.id AND selected.project_id=t.project_id AND selected.script_id=t.script_id AND selected.state='生成成功' AND coalesce(selected.file_path,'')<>''),(SELECT latest.id FROM toonflow.videos latest WHERE latest.video_track_id=t.id AND latest.project_id=t.project_id AND latest.script_id=t.script_id AND latest.state='生成成功' AND coalesce(latest.file_path,'')<>'' ORDER BY latest.time DESC,latest.id DESC LIMIT 1)) WHERE t.project_id=$1 AND t.script_id=$2 ORDER BY coalesce((SELECT min(coalesce(s.index,2147483647)) FROM toonflow.storyboards s WHERE s.track_id=t.id),2147483647),t.sort_order,t.id")
            .bind(project_id)
            .bind(script_id)
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| {
                tracing::error!(project_id, script_id, %error, "failed to snapshot export sources");
                AppError::internal("failed to snapshot export sources")
            })?
    } else {
        let rows: Vec<i64> = sqlx::query_scalar("SELECT v.id FROM toonflow.videos v JOIN toonflow.video_tracks t ON t.id=v.video_track_id WHERE v.id=ANY($3) AND v.project_id=$1 AND v.script_id=$2 AND v.state='生成成功' AND coalesce(v.file_path,'')<>'' ORDER BY coalesce((SELECT min(coalesce(s.index,2147483647)) FROM toonflow.storyboards s WHERE s.track_id=t.id),2147483647),t.sort_order,t.id")
            .bind(project_id)
            .bind(script_id)
            .bind(requested_video_ids)
            .fetch_all(&mut **tx)
            .await
            .map_err(|error| {
                tracing::error!(project_id, script_id, %error, "failed to snapshot selected export sources");
                AppError::internal("failed to snapshot export sources")
            })?;
        let requested_count = requested_video_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len();
        if rows.len() != requested_count {
            return Err(AppError::bad_request(
                "部分视频不存在、尚未生成成功或不属于当前剧集",
            ));
        }
        rows
    };
    if ordered_video_ids.is_empty() {
        return Err(AppError::bad_request("请先为每条轨道选择已生成的视频"));
    }
    // Hold the successful source rows through the task/outbox commit. A media
    // delete needs FOR UPDATE on the same rows and therefore cannot enqueue
    // object cleanup between this snapshot and durable job visibility.
    let locked_rows: Vec<(i64, String, String, i32, i32, Option<i32>)> = sqlx::query_as(
        "SELECT video.id,video.file_path,
                track.transition_type,track.transition_duration_ms,
                track.trim_start_ms,track.trim_end_ms
         FROM toonflow.videos video
         JOIN toonflow.video_tracks track ON track.id=video.video_track_id
         WHERE video.id=ANY($3) AND video.project_id=$1 AND video.script_id=$2
           AND track.project_id=$1 AND track.script_id=$2
           AND video.state='生成成功' AND coalesce(video.file_path,'')<>''
         ORDER BY video.id
         FOR SHARE OF video",
    )
    .bind(project_id)
    .bind(script_id)
    .bind(&ordered_video_ids)
    .fetch_all(&mut **tx)
    .await
    .map_err(|error| {
        tracing::error!(project_id, script_id, %error, "failed to lock export sources");
        AppError::internal("failed to snapshot export sources")
    })?;
    if locked_rows.len() != ordered_video_ids.len() {
        return Err(AppError::bad_request(
            "部分视频在提交合并任务前已变化，请刷新后重试",
        ));
    }
    let mut locked_by_id = locked_rows
        .into_iter()
        .map(
            |(
                video_id,
                file_path,
                transition_type,
                transition_duration_ms,
                trim_start_ms,
                trim_end_ms,
            )| {
                (
                    video_id,
                    (
                        file_path,
                        transition_type,
                        transition_duration_ms,
                        trim_start_ms,
                        trim_end_ms,
                    ),
                )
            },
        )
        .collect::<std::collections::HashMap<_, _>>();
    let rows = ordered_video_ids
        .into_iter()
        .map(|video_id| {
            locked_by_id.remove(&video_id).map(
                |(
                    file_path,
                    transition_type,
                    transition_duration_ms,
                    trim_start_ms,
                    trim_end_ms,
                )| {
                    (
                        video_id,
                        file_path,
                        transition_type,
                        transition_duration_ms,
                        trim_start_ms,
                        trim_end_ms,
                    )
                },
            )
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| AppError::bad_request("视频合并顺序已变化，请刷新后重试"))?;
    let expected_prefix = format!("toonflow/{project_id}/assets/");
    if rows.iter().any(|(_, file_path, ..)| {
        crate::toonflow_storage::asset_object_key(file_path)
            .is_none_or(|key| !key.starts_with(&expected_prefix))
    }) {
        return Err(AppError::bad_request(
            "源视频尚未归档到当前项目存储，请重新生成或导入后再合成",
        ));
    }
    Ok(rows
        .into_iter()
        .map(
            |(
                video_id,
                file_path,
                transition_type,
                transition_duration_ms,
                trim_start_ms,
                trim_end_ms,
            )| {
                VideoExportSource {
                    video_id,
                    file_path,
                    transition_type,
                    transition_duration_ms: i64::from(transition_duration_ms),
                    trim_start_ms: i64::from(trim_start_ms),
                    trim_end_ms: trim_end_ms.map(i64::from),
                }
            },
        )
        .collect())
}

async fn update_export_progress(
    pool: &sqlx::PgPool,
    job_id: i64,
    task_id: i64,
    lease_token: Uuid,
    current: i32,
    total: i32,
) {
    let _ = sqlx::query(
        "UPDATE toonflow.tasks tasks
         SET progress_current=$4,progress_total=$5
         WHERE tasks.id=$2 AND tasks.state='running'
           AND EXISTS(
             SELECT 1 FROM toonflow.distributed_jobs jobs
             WHERE jobs.id=$1 AND jobs.task_id=$2 AND jobs.state='running'
               AND jobs.lease_token=$3 AND jobs.lease_until > now()
           )",
    )
    .bind(job_id)
    .bind(task_id)
    .bind(lease_token)
    .bind(current)
    .bind(total)
    .execute(pool)
    .await;
}

async fn register_staging_export(
    pool: &sqlx::PgPool,
    job_id: i64,
    task_id: i64,
    lease_token: Uuid,
    file_path: &str,
) -> Result<(), String> {
    let updated = sqlx::query(
        "UPDATE toonflow.distributed_jobs
         SET result=jsonb_build_object('stagingObjectPath',$4),updated_at=now()
         WHERE id=$1 AND task_id=$2 AND state='running'
           AND lease_token=$3 AND lease_until > now()",
    )
    .bind(job_id)
    .bind(task_id)
    .bind(lease_token)
    .bind(file_path)
    .execute(pool)
    .await
    .map_err(|error| error.to_string())?;
    if updated.rows_affected() != 1 {
        return Err("任务租约已失效，拒绝上传过期 Worker 的结果".into());
    }
    Ok(())
}

async fn run_export(
    pool: sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    job_id: i64,
    task_id: i64,
    lease_token: Uuid,
    sources: Vec<VideoExportSource>,
) -> Result<ExportArtifact, String> {
    if sources.is_empty() {
        return Err("任务缺少冻结的有序源视频快照，拒绝执行不可重复的导出".into());
    }
    let total = i32::try_from(sources.len()).map_err(|_| "源视频数量过多".to_string())?;
    update_export_progress(&pool, job_id, task_id, lease_token, 0, total).await;
    let work_dir = std::env::temp_dir().join(format!(
        "rust-toon/toonflow/{project_id}/exports/work-{task_id}-{lease_token}"
    ));
    tokio::fs::create_dir_all(&work_dir)
        .await
        .map_err(|error| error.to_string())?;
    let work_dir = ExportWorkDir(work_dir);
    let mut files = Vec::new();
    let mut source_bytes = 0_u64;
    let max_job_source_bytes = job_source_size_limit();
    for (index, source) in sources.iter().enumerate() {
        let file = materialize_video(&source.file_path, work_dir.path(), index).await?;
        let file_bytes = tokio::fs::metadata(&file)
            .await
            .map_err(|error| format!("读取源视频大小失败：{error}"))?
            .len();
        source_bytes = source_bytes
            .checked_add(file_bytes)
            .ok_or_else(|| "成片源视频总大小溢出".to_string())?;
        if source_bytes > max_job_source_bytes {
            return Err(format!(
                "成片源视频总大小超过任务限制（最大 {max_job_source_bytes} 字节）"
            ));
        }
        files.push(file);
        update_export_progress(
            &pool,
            job_id,
            task_id,
            lease_token,
            i32::try_from(index + 1).unwrap_or(total),
            total,
        )
        .await;
    }
    let (first_width, first_height) = probe_dimensions(&files[0]).await?;
    let (target_width, target_height) = if first_width >= first_height {
        (1920, 1080)
    } else {
        (1080, 1920)
    };
    let mut clips = Vec::with_capacity(files.len());
    for (file, source) in files.iter().zip(sources.iter()) {
        let duration = probe_duration(file)
            .await
            .map_err(|reason| format!("源视频 {} 无法探测时长：{reason}", source.video_id))?;
        let has_audio = probe_has_audio(file)
            .await
            .map_err(|reason| format!("源视频 {} 无法探测音轨：{reason}", source.video_id))?;
        clips.push(TimelineClip {
            duration_ms: (duration * 1000.0).round() as i64,
            has_audio,
            trim_start_ms: source.trim_start_ms,
            trim_end_ms: source.trim_end_ms,
            transition_type: source.transition_type.clone(),
            transition_duration_ms: source.transition_duration_ms,
        });
    }
    let plan = build_timeline_plan(&clips, target_width, target_height)?;
    let transition_count = clips
        .iter()
        .skip(1)
        .filter(|clip| matches!(clip.transition_type.as_str(), "dissolve" | "audio_bridge"))
        .count();

    let output_path = work_dir
        .path()
        .join(format!("{script_id}_{task_id}_{lease_token}.mp4"));
    if let Some(parent) = output_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| error.to_string())?;
    }
    let mut command = tokio::process::Command::new("ffmpeg");
    command
        .args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-y",
            "-fflags",
            "+genpts",
        ])
        .stdin(Stdio::null());
    for file in &files {
        command.arg("-i").arg(file);
    }
    for _ in 0..plan.silent_inputs {
        command.args([
            "-f",
            "lavfi",
            "-i",
            "anullsrc=channel_layout=stereo:sample_rate=48000",
        ]);
    }
    command
        .args([
            "-filter_complex",
            &plan.filter_graph,
            "-map",
            "[vout]",
            "-map",
            "[aout]",
            "-r",
            "30",
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "23",
            "-c:a",
            "aac",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-b:a",
            "192k",
            "-movflags",
            "+faststart",
        ])
        .arg(&output_path);
    let output = command_output(&mut command, media_timeout(), "成片导出").await?;
    if !output.status.success() {
        return Err(format!(
            "成片导出失败：{}",
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .last()
                .unwrap_or("FFmpeg 执行失败")
        ));
    }
    let object_name = format!("task-{task_id}-lease-{lease_token}");
    let staging_path =
        crate::toonflow_storage::asset_file_path_named(project_id, "exports", &object_name, "mp4")?;
    register_staging_export(&pool, job_id, task_id, lease_token, &staging_path).await?;
    let stored = crate::toonflow_storage::persist_asset_file_named(
        project_id,
        "exports",
        &object_name,
        "mp4",
        &output_path,
    )
    .await?;
    Ok(ExportArtifact {
        object_path: object_path_from_file_path(&stored),
        file_path: stored,
        source_video_ids: sources.into_iter().map(|source| source.video_id).collect(),
        metadata: json!({
            "width": target_width,
            "height": target_height,
            "sourceCount": files.len(),
            "durationMs": plan.total_duration_ms,
            "transitionCount": transition_count,
            "container": "mp4",
            "videoCodec": "h264",
            "audioCodec": "aac",
        }),
    })
}

async fn finalize_export(
    pool: &sqlx::PgPool,
    job_id: i64,
    lease_token: Uuid,
    task_id: i64,
    payload: &VideoExportJobPayload,
    artifact: &ExportArtifact,
) -> Result<Value, String> {
    let created_by = Uuid::parse_str(&payload.created_by)
        .map_err(|error| format!("无效的任务创建者：{error}"))?;
    let mut tx = pool.begin().await.map_err(|error| error.to_string())?;
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM toonflow.distributed_jobs
         WHERE id=$1 AND task_id=$2 AND state='running'
           AND lease_token=$3 AND lease_until > now()
         FOR UPDATE",
    )
    .bind(job_id)
    .bind(task_id)
    .bind(lease_token)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "任务租约已失效，拒绝提交过期 Worker 的结果".to_string())?;
    sqlx::query_scalar::<_, i64>(
        "SELECT id FROM toonflow.scripts WHERE id=$1 AND project_id=$2 FOR UPDATE",
    )
    .bind(payload.script_id)
    .bind(payload.project_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| error.to_string())?
    .ok_or_else(|| "导出完成前项目或剧集已被删除".to_string())?;

    let (render_id, version, file_path, replayed) = if let Some(existing) =
        sqlx::query_as::<_, (i64, i32, String)>(
            "SELECT id,version,file_path FROM toonflow.episode_renders WHERE export_task_id=$1",
        )
        .bind(task_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|error| error.to_string())?
    {
        (existing.0, existing.1, existing.2, true)
    } else {
        let version: i32 = sqlx::query_scalar(
            "SELECT COALESCE(max(version),0)+1
             FROM toonflow.episode_renders
             WHERE project_id=$1 AND script_id=$2",
        )
        .bind(payload.project_id)
        .bind(payload.script_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        sqlx::query(
            "UPDATE toonflow.episode_renders
             SET is_current=false,updated_at=now()
             WHERE project_id=$1 AND script_id=$2 AND is_current",
        )
        .bind(payload.project_id)
        .bind(payload.script_id)
        .execute(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        let render_id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.episode_renders
             (project_id,script_id,version,object_path,file_path,cover_path,status,
              source_video_ids,metadata,is_current,created_by,export_task_id)
             VALUES($1,$2,$3,$4,$5,NULL,'ready',$6,$7,true,$8,$9)
             RETURNING id",
        )
        .bind(payload.project_id)
        .bind(payload.script_id)
        .bind(version)
        .bind(&artifact.object_path)
        .bind(&artifact.file_path)
        .bind(&artifact.source_video_ids)
        .bind(&artifact.metadata)
        .bind(created_by)
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| error.to_string())?;
        (render_id, version, artifact.file_path.clone(), false)
    };
    let result = json!({
        "scriptId": payload.script_id,
        "videoIds": artifact.source_video_ids,
        "url": file_path,
        "episodeRenderId": render_id,
        "version": version,
        "replayed": replayed,
    });
    let task_update = sqlx::query(
        "UPDATE toonflow.tasks
         SET state='success',related_objects=$2,reason=NULL
         WHERE id=$1 AND state IN ('running','success')",
    )
    .bind(task_id)
    .bind(result.to_string())
    .execute(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;
    if task_update.rows_affected() != 1 {
        return Err("导出任务不存在或已结束，拒绝写入伪成功结果".into());
    }
    let job_update = sqlx::query(
        "UPDATE toonflow.distributed_jobs
         SET state='succeeded',result=$4,completed_at=now(),updated_at=now(),
             lease_owner=NULL,lease_token=NULL,lease_until=NULL,heartbeat_at=NULL
         WHERE id=$1 AND task_id=$2 AND state='running' AND lease_token=$3",
    )
    .bind(job_id)
    .bind(task_id)
    .bind(lease_token)
    .bind(&result)
    .execute(&mut *tx)
    .await
    .map_err(|error| error.to_string())?;
    if job_update.rows_affected() != 1 {
        return Err("任务租约已失效，拒绝提交过期 Worker 的结果".into());
    }
    tx.commit().await.map_err(|error| error.to_string())?;
    Ok(result)
}

/// Execute one durable export after a worker has acquired the matching lease.
/// The lease token is checked in the same transaction that publishes the
/// episode render, providing a fencing guarantee for expired workers.
pub async fn execute_distributed_export(
    pool: &sqlx::PgPool,
    job_id: i64,
    task_id: i64,
    lease_token: Uuid,
    payload: Value,
) -> Result<Value, String> {
    let payload: VideoExportJobPayload =
        serde_json::from_value(payload).map_err(|error| format!("无效的成片任务参数：{error}"))?;
    if let Some((object_path, file_path, source_video_ids, metadata)) =
        sqlx::query_as::<_, (String, String, Vec<i64>, Value)>(
            "SELECT object_path,file_path,source_video_ids,metadata
         FROM toonflow.episode_renders WHERE export_task_id=$1",
        )
        .bind(task_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| error.to_string())?
    {
        let artifact = ExportArtifact {
            object_path,
            file_path,
            source_video_ids,
            metadata,
        };
        return finalize_export(pool, job_id, lease_token, task_id, &payload, &artifact).await;
    }

    if !ffmpeg_available().await {
        return Err("Worker 未安装可用的 FFmpeg/FFprobe".into());
    }
    let artifact = run_export(
        pool.clone(),
        payload.project_id,
        payload.script_id,
        job_id,
        task_id,
        lease_token,
        payload.sources.clone(),
    )
    .await?;
    // Do not delete the object here when finalization reports an error: a
    // commit acknowledgement can be lost after PostgreSQL has already made the
    // episode render visible. Definite failed attempts retain their staging
    // path in the durable job row; JobStore::fail/reap_expired moves that path
    // to the reference-aware cleanup queue under the same lease fence.
    finalize_export(pool, job_id, lease_token, task_id, &payload, &artifact).await
}

pub async fn export(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<ExportRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let created_by = ensure_project_access(&state.pool, &user, request.project_id).await?;
    ensure_script_in_project(&state.pool, request.project_id, request.script_id).await?;
    let mut tx = state.pool.begin().await.map_err(|error| {
        tracing::error!(error = %error, "failed to start video export transaction");
        AppError::internal("failed to create export task")
    })?;
    let sources = resolve_export_sources(
        &mut tx,
        request.project_id,
        request.script_id,
        &request.video_ids,
    )
    .await?;
    let video_ids = sources
        .iter()
        .map(|source| source.video_id)
        .collect::<Vec<_>>();
    let related_objects = json!({
        "scriptId": request.script_id,
        "videoIds": &video_ids,
        "sources": &sources,
    })
    .to_string();
    let input = json!({
        "projectId": request.project_id,
        "scriptId": request.script_id,
        "videoIds": &video_ids,
    });
    let payload = VideoExportJobPayload {
        project_id: request.project_id,
        script_id: request.script_id,
        video_ids: video_ids.clone(),
        sources,
        created_by: created_by.to_string(),
    };
    let payload = serde_json::to_value(payload)
        .map_err(|_| AppError::internal("failed to serialize export job"))?;
    let task_id: i64 = sqlx::query_scalar("INSERT INTO toonflow.tasks(project_id,task_class,related_objects,model,description,state,start_time,input,progress_current,progress_total) VALUES($1,'videoExport',$2,'ffmpeg','合并选中视频为最终成片','running',(extract(epoch from clock_timestamp())*1000)::bigint,$3,0,$4) RETURNING id")
        .bind(request.project_id)
        .bind(related_objects)
        .bind(input)
        .bind(i32::try_from(video_ids.len()).unwrap_or(i32::MAX))
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| {
            tracing::error!(error = %error, "failed to create video export task");
            AppError::internal("failed to create export task")
        })?;
    sqlx::query(
        "INSERT INTO toonflow.distributed_jobs(message_id,task_id,kind,trace_id,trace_context,payload)
         VALUES($1,$2,$3,$4,$5,$6)",
    )
    .bind(Uuid::new_v4())
    .bind(task_id)
    .bind(VIDEO_EXPORT_JOB_KIND)
    .bind(
        rust_toon_framework_telemetry::current_trace_id()
            .unwrap_or_else(|| format!("video-export-{task_id}")),
    )
    .bind(json!(
        rust_toon_framework_telemetry::current_trace_context()
    ))
    .bind(payload)
    .execute(&mut *tx)
    .await
    .map_err(|error| {
        tracing::error!(task_id, error = %error, "failed to enqueue video export job");
        AppError::internal("failed to enqueue export task")
    })?;
    tx.commit().await.map_err(|error| {
        tracing::error!(task_id, error = %error, "failed to commit video export job");
        AppError::internal("failed to create export task")
    })?;
    Ok(Json(ApiResponse::new(
        json!({"taskId":task_id,"state":"running"}),
    )))
}

#[cfg(test)]
mod tests {
    use super::{TimelineClip, build_timeline_plan, ip_is_non_public, resolve_export_sources};
    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
    use serde_json::json;
    use std::{
        net::{IpAddr, Ipv4Addr, Ipv6Addr},
        time::Duration,
    };

    fn clip(duration_ms: i64, transition_type: &str) -> TimelineClip {
        TimelineClip {
            duration_ms,
            has_audio: true,
            trim_start_ms: 0,
            trim_end_ms: None,
            transition_type: transition_type.into(),
            transition_duration_ms: 600,
        }
    }

    #[test]
    fn hard_cuts_concat_video_and_audio_in_one_pass() {
        let plan = build_timeline_plan(&[clip(4_000, "cut"), clip(5_000, "cut")], 1920, 1080)
            .expect("cut timeline");
        assert_eq!(plan.total_duration_ms, 9_000);
        assert_eq!(plan.silent_inputs, 0);
        assert!(plan.filter_graph.contains("concat=n=2:v=1:a=0"));
        assert!(plan.filter_graph.contains("concat=n=2:v=0:a=1"));
        assert!(!plan.filter_graph.contains("xfade"));
        assert!(!plan.filter_graph.contains("acrossfade"));
    }

    #[test]
    fn dissolve_crossfades_video_and_audio_with_overlap() {
        let plan = build_timeline_plan(&[clip(4_000, "cut"), clip(5_000, "dissolve")], 1920, 1080)
            .expect("dissolve timeline");
        assert_eq!(plan.total_duration_ms, 8_400);
        assert!(
            plan.filter_graph
                .contains("xfade=transition=fade:duration=0.600:offset=3.400")
        );
        assert!(plan.filter_graph.contains("acrossfade=d=0.600"));
    }

    #[test]
    fn audio_bridge_keeps_hard_video_cut_and_leads_audio() {
        let plan = build_timeline_plan(
            &[clip(4_000, "cut"), clip(5_000, "audio_bridge")],
            1920,
            1080,
        )
        .expect("audio bridge timeline");
        // Video stays a hard cut; the tail pad covers the audio shifted early.
        assert_eq!(plan.total_duration_ms, 9_000);
        assert!(!plan.filter_graph.contains("xfade"));
        assert!(plan.filter_graph.contains("acrossfade=d=0.600"));
        assert!(plan.filter_graph.contains("apad,atrim=end=9.000"));
    }

    #[test]
    fn generation_only_transition_types_fall_back_to_hard_cut() {
        for transition in ["continuous", "action_bridge", "empty_shot", "match_cut"] {
            let plan =
                build_timeline_plan(&[clip(4_000, "cut"), clip(5_000, transition)], 1920, 1080)
                    .expect("generation-only transition");
            assert_eq!(plan.total_duration_ms, 9_000, "{transition}");
            assert!(!plan.filter_graph.contains("xfade"), "{transition}");
        }
    }

    #[test]
    fn silent_clips_use_generated_silence_inputs() {
        let mut silent = clip(3_000, "cut");
        silent.has_audio = false;
        let plan = build_timeline_plan(&[clip(4_000, "cut"), silent], 1920, 1080)
            .expect("silent clip timeline");
        assert_eq!(plan.silent_inputs, 1);
        assert!(plan.filter_graph.contains("[2:a]atrim=end=3.000"));
    }

    #[test]
    fn trim_points_apply_and_validate_against_source_duration() {
        let mut trimmed = clip(6_000, "cut");
        trimmed.trim_start_ms = 1_000;
        trimmed.trim_end_ms = Some(5_000);
        let plan = build_timeline_plan(&[trimmed], 1920, 1080).expect("trimmed timeline");
        assert_eq!(plan.total_duration_ms, 4_000);
        assert!(plan.filter_graph.contains("trim=start=1.000:end=5.000"));
        assert!(plan.filter_graph.contains("atrim=start=1.000:end=5.000"));

        let mut over_trimmed = clip(2_000, "cut");
        over_trimmed.trim_start_ms = 2_500;
        assert!(build_timeline_plan(&[over_trimmed], 1920, 1080).is_err());

        let mut tiny = clip(2_000, "cut");
        tiny.trim_end_ms = Some(50);
        assert!(build_timeline_plan(&[tiny], 1920, 1080).is_err());
    }

    #[test]
    fn transition_overlap_is_clamped_to_shorter_side() {
        let mut long_dissolve = clip(1_000, "dissolve");
        long_dissolve.transition_duration_ms = 5_000;
        let plan = build_timeline_plan(&[clip(4_000, "cut"), long_dissolve], 1920, 1080)
            .expect("clamped dissolve timeline");
        // 90% of the shorter 1s clip caps the overlap at 900ms.
        assert_eq!(plan.total_duration_ms, 4_100);
        assert!(
            plan.filter_graph
                .contains("xfade=transition=fade:duration=0.900:offset=3.100")
        );
    }

    #[test]
    fn tiny_transitions_degrade_to_hard_cut() {
        let mut almost_cut = clip(5_000, "dissolve");
        almost_cut.transition_duration_ms = 10;
        let plan = build_timeline_plan(&[clip(4_000, "cut"), almost_cut], 1920, 1080)
            .expect("tiny transition timeline");
        assert_eq!(plan.total_duration_ms, 9_000);
        assert!(!plan.filter_graph.contains("xfade"));
    }

    #[test]
    fn external_video_guard_rejects_internal_and_reserved_networks() {
        for address in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
            IpAddr::V4(Ipv4Addr::new(169, 254, 169, 254)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 1)),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            "fd00::1".parse().expect("unique-local IPv6"),
            "fe80::1".parse().expect("link-local IPv6"),
        ] {
            assert!(ip_is_non_public(address), "{address} must be rejected");
        }
        assert!(!ip_is_non_public(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!ip_is_non_public(
            "2606:4700:4700::1111".parse().expect("public IPv6")
        ));
    }

    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn source_snapshot_blocks_video_delete_until_job_enqueue_commits() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();

        let project_id = 9_700_001_i64;
        let script_id = 9_700_002_i64;
        let track_id = 9_700_003_i64;
        let video_id = 9_700_004_i64;
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.projects(id,name,create_time,update_time)
             VALUES($1,'export source lock test',0,0)",
        )
        .bind(project_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.scripts(id,name,project_id,create_time)
             VALUES($1,'episode',$2,0)",
        )
        .bind(script_id)
        .bind(project_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.video_tracks(id,project_id,script_id,sort_order)
             VALUES($1,$2,$3,0)",
        )
        .bind(track_id)
        .bind(project_id)
        .bind(script_id)
        .execute(&pool)
        .await
        .unwrap();
        let file_path = format!(
            "/toonflow/assets/files/toonflow/{project_id}/assets/videos/source-lock-test.mp4"
        );
        sqlx::query(
            "INSERT INTO toonflow.videos(
               id,file_path,state,script_id,project_id,video_track_id,time
             ) VALUES($1,$2,'生成成功',$3,$4,$5,0)",
        )
        .bind(video_id)
        .bind(&file_path)
        .bind(script_id)
        .bind(project_id)
        .bind(track_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE toonflow.video_tracks SET video_id=$2 WHERE id=$1")
            .bind(track_id)
            .bind(video_id)
            .execute(&pool)
            .await
            .unwrap();

        let mut enqueue_tx = pool.begin().await.unwrap();
        let sources = resolve_export_sources(&mut enqueue_tx, project_id, script_id, &[video_id])
            .await
            .unwrap();
        assert_eq!(sources.len(), 1);
        let task_id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.tasks(project_id,task_class,description,state)
             VALUES($1,'videoExport','source lock regression','running')
             RETURNING id",
        )
        .bind(project_id)
        .fetch_one(&mut *enqueue_tx)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.distributed_jobs(task_id,kind,payload)
             VALUES($1,'toon.video_export',$2)",
        )
        .bind(task_id)
        .bind(json!({ "sources": sources }))
        .execute(&mut *enqueue_tx)
        .await
        .unwrap();

        let mut blocked_delete_tx = pool.begin().await.unwrap();
        sqlx::query("SET LOCAL lock_timeout = '250ms'")
            .execute(&mut *blocked_delete_tx)
            .await
            .unwrap();
        let lock_error = sqlx::query_scalar::<_, String>(
            "SELECT file_path FROM toonflow.videos WHERE id=$1 FOR UPDATE",
        )
        .bind(video_id)
        .fetch_one(&mut *blocked_delete_tx)
        .await
        .expect_err("video deletion acquired FOR UPDATE before the export job was committed");
        assert_eq!(
            lock_error
                .as_database_error()
                .and_then(|error| error.code())
                .as_deref(),
            Some("55P03"),
            "the delete probe must fail specifically because the source row is locked"
        );
        blocked_delete_tx.rollback().await.unwrap();

        enqueue_tx.commit().await.unwrap();
        let mut delete_tx = pool.begin().await.unwrap();
        let locked: String = tokio::time::timeout(
            Duration::from_secs(5),
            sqlx::query_scalar("SELECT file_path FROM toonflow.videos WHERE id=$1 FOR UPDATE")
                .bind(video_id)
                .fetch_one(&mut *delete_tx),
        )
        .await
        .expect("video deletion remained blocked after enqueue commit")
        .unwrap();
        assert_eq!(locked, file_path);
        delete_tx.rollback().await.unwrap();

        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
