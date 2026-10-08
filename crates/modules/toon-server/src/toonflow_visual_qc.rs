//! P1 视觉质检：结构化质检结论与定向修复指令。
//!
//! 引擎部分是纯函数：质检提示词构建、结论解析与修复指令生成，全部可单测。
//! 供应商调用复用 `ai_client::project_text_tools` 的多模态消息通道
//! （`image_url` 内容分片），模型路由沿用 `productionAgent:*` 键约定。
//! 接入点按批次落地：先资产/分镜图片完成路径，后视频抽帧（需 worker
//! 侧 FFmpeg，见 `toonflow_video_quality` 的分布式作业模式）。

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;

/// 每项生成内容最多自动重试次数；两次仍失败保留全部快照交人工处理。
pub(crate) const MAX_VISUAL_QC_RETRIES: usize = 2;

pub(crate) const VISUAL_QC_AGENT_KEY: &str = "productionAgent:visualQcAgent";

/// 图片质检维度与中文说明。新增维度前先确认提示词与修复指令都能消费它。
pub(crate) fn qc_check_catalog() -> Vec<(&'static str, &'static str)> {
    vec![
        ("character_count", "画面人物数量与预期出镜主体一致"),
        ("identity", "人物身份、发型、面部特征与参考图一致"),
        ("scene", "场景与场次/场景状态约束一致"),
        ("costume", "服装与造型描述一致"),
        ("action", "动作与画面描述一致"),
        ("proportion", "人体比例与透视正常，无肢体畸变"),
        ("text_watermark", "画面没有乱码文字或水印"),
    ]
}

/// 视频抽帧质检维度（对照结构化镜头与首帧参考）。
pub(crate) fn video_qc_check_catalog() -> Vec<(&'static str, &'static str)> {
    vec![
        ("first_frame_similarity", "首帧与参考清单中的首帧内容一致"),
        ("identity_stability", "人物身份与造型在所有帧之间保持一致"),
        ("motion_direction", "人物动作方向与画面描述一致且连贯"),
        ("camera_movement", "镜头运动符合指定的运镜要求"),
        ("flicker_distortion", "无闪烁、形变、肢体畸变或画面抖动"),
        ("text_watermark", "画面没有乱码文字或水印"),
    ]
}

fn catalog_names<'a>(catalog: &'a [(&'a str, &'a str)]) -> Vec<&'a str> {
    catalog.iter().map(|(name, _)| *name).collect()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VisualQcFailure {
    pub(crate) check: String,
    pub(crate) evidence: String,
    pub(crate) confidence: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VisualQcReport {
    pub(crate) passed: bool,
    pub(crate) failures: Vec<VisualQcFailure>,
    pub(crate) summary: String,
}

fn qc_system_prompt_for(catalog: &[(&str, &str)]) -> String {
    let checks = catalog
        .iter()
        .map(|(name, description)| format!("- {name}: {description}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "你是动画生成内容的视觉质检员。对照期望说明逐项检查提供的图片，只能使用以下维度：\n{checks}\n\
         必须调用 submit_visual_qc 提交结论：每个不通过的维度给出 evidence（指明画面中具体哪里不符，\
         不得泛泛而谈）与 0 到 1 的 confidence；全部通过时 failures 为空数组并在 summary 说明。\
         不得发明维度，不得因为偏好风格而判失败。"
    )
}

fn qc_tool_for(catalog: &[(&str, &str)]) -> Value {
    json!({
        "type":"function",
        "function":{
            "name":"submit_visual_qc",
            "description":"提交结构化视觉质检结论。",
            "parameters":{
                "type":"object",
                "properties":{
                    "passed":{"type":"boolean"},
                    "summary":{"type":"string"},
                    "failures":{
                        "type":"array",
                        "items":{
                            "type":"object",
                            "properties":{
                                "check":{"type":"string","enum":catalog_names(catalog)},
                                "evidence":{"type":"string"},
                                "confidence":{"type":"number"}
                            },
                            "required":["check","evidence","confidence"]
                        }
                    }
                },
                "required":["passed","summary","failures"]
            }
        }
    })
}

/// 解析模型提交的质检结论。维度名、置信度范围与 passed 一致性都严格校验：
/// 质检结论驱动自动重试，坏数据宁可拒绝也不能放行。
pub(crate) fn parse_qc_verdict(value: &Value) -> Result<VisualQcReport, String> {
    parse_verdict_for(value, &qc_check_catalog())
}

pub(crate) fn parse_video_qc_verdict(value: &Value) -> Result<VisualQcReport, String> {
    parse_verdict_for(value, &video_qc_check_catalog())
}

fn parse_verdict_for(
    value: &Value,
    catalog: &[(&str, &str)],
) -> Result<VisualQcReport, String> {
    let valid_checks = catalog_names(catalog);
    let passed = value
        .get("passed")
        .and_then(Value::as_bool)
        .ok_or("质检结论缺少 passed 布尔值")?;
    let summary = value
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let mut failures = Vec::new();
    for item in value
        .get("failures")
        .and_then(Value::as_array)
        .ok_or("质检结论缺少 failures 数组")?
    {
        let check = item
            .get("check")
            .and_then(Value::as_str)
            .ok_or("质检失败项缺少 check")?;
        if !valid_checks.contains(&check) {
            return Err(format!("未知质检维度：{check}"));
        }
        let evidence = item
            .get("evidence")
            .and_then(Value::as_str)
            .ok_or("质检失败项缺少 evidence")?;
        if evidence.trim().is_empty() {
            return Err(format!("质检维度 {check} 的 evidence 为空"));
        }
        let confidence = item
            .get("confidence")
            .and_then(Value::as_f64)
            .ok_or("质检失败项缺少 confidence")?;
        if !(0.0..=1.0).contains(&confidence) {
            return Err(format!("质检维度 {check} 的 confidence 超出 0 到 1"));
        }
        failures.push(VisualQcFailure {
            check: check.to_string(),
            evidence: evidence.trim().to_string(),
            confidence,
        });
    }
    if passed && !failures.is_empty() {
        return Err("passed 为真但存在失败项".into());
    }
    if !passed && failures.is_empty() {
        return Err("passed 为假但没有失败项".into());
    }
    Ok(VisualQcReport {
        passed,
        failures,
        summary,
    })
}

/// 从失败项生成定向修复指令，只针对失败维度，避免重试时重写全部要求。
pub(crate) fn repair_instructions(report: &VisualQcReport) -> Option<String> {
    repair_instructions_for(report, &qc_check_catalog())
}

pub(crate) fn repair_instructions_for(
    report: &VisualQcReport,
    catalog: &[(&str, &str)],
) -> Option<String> {
    if report.passed || report.failures.is_empty() {
        return None;
    }
    let items = report
        .failures
        .iter()
        .map(|failure| {
            let description = catalog
                .iter()
                .find(|(name, _)| *name == failure.check)
                .map(|(_, description)| *description)
                .unwrap_or("生成内容与期望不符");
            format!(
                "- {}（{}）：{}",
                description, failure.check, failure.evidence
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "上一版生成未通过视觉质检，只修复以下问题，保持其余构图与内容不变：\n{items}"
    ))
}

/// 从资产结构化输入编译质检期望说明。画风只约束技法：质检员不得因为
/// 风格偏好判失败，这与生成侧“用户事实优先、画风只约束审美”一致。
pub(crate) fn asset_image_expectations(
    asset_type: &str,
    description: &str,
    appearance_anchor: &str,
    style: &str,
) -> String {
    format!(
        "资产类型：{asset_type}\n内容要求：{description}\n{appearance_anchor}\n画风：{style}（只约束绘画技法与整体观感，不得改变上述内容事实）"
    )
}

/// 从分镜结构化输入编译质检期望说明：画面描述、景别/运镜（若有）与
/// 必须一致的参考资产清单。空字段不产生条目。
pub(crate) fn storyboard_expectations(
    description: &str,
    shot_size: &str,
    camera_move: &str,
    time_of_day: &str,
    carried_objects: &str,
    assets: &[(&str, &str)],
) -> String {
    let mut sections = vec![format!("画面描述：{description}")];
    if !shot_size.is_empty() {
        sections.push(format!("景别：{shot_size}"));
    }
    if !camera_move.is_empty() {
        sections.push(format!("运镜：{camera_move}"));
    }
    if !time_of_day.is_empty() {
        sections.push(format!("时间：{time_of_day}（光线、色温与阴影必须与该时间一致）"));
    }
    if !carried_objects.is_empty() {
        sections.push(format!(
            "随身物件（必须出现在画面中且年代特征正确）：{carried_objects}"
        ));
    }
    if !assets.is_empty() {
        let list = assets
            .iter()
            .map(|(name, kind)| format!("{name}（{kind}）"))
            .collect::<Vec<_>>()
            .join("、");
        sections.push(format!("人物与场景必须与以下参考资产一致：{list}"));
    }
    sections.join("\n")
}

/// 视频抽帧质检的期望说明：镜头描述汇总、运镜与首帧参考摘要。
pub(crate) fn video_frame_expectations(
    shots_summary: &str,
    camera_moves: &str,
    first_frame_summary: &str,
) -> String {
    let mut sections = vec![format!("镜头内容：{shots_summary}")];
    if !camera_moves.is_empty() {
        sections.push(format!("运镜要求：{camera_moves}"));
    }
    if !first_frame_summary.is_empty() {
        sections.push(format!("首帧参考：{first_frame_summary}（视频首帧应与其内容一致）"));
    }
    sections.join("\n")
}

/// 对一张生成图执行视觉质检。期望说明由调用方从结构化输入编译
/// （资产/造型描述、参考图清单、镜头约束等）。
pub(crate) async fn evaluate_image(
    pool: &PgPool,
    project_id: i64,
    image_path: &str,
    expectations: &str,
) -> Result<VisualQcReport, String> {
    let data_url =
        crate::toonflow_storage::image_data_url(image_path).await?;
    evaluate_image_parts(
        pool,
        project_id,
        vec![data_url],
        expectations,
        &qc_check_catalog(),
    )
    .await
}

/// 对视频抽出的多帧执行视觉质检。帧以 data URL 传入（worker 从本地
/// 抽帧文件直接编码，不依赖对象存储回读）。
pub(crate) async fn evaluate_frames(
    pool: &PgPool,
    project_id: i64,
    frame_data_urls: Vec<String>,
    expectations: &str,
) -> Result<VisualQcReport, String> {
    evaluate_image_parts(
        pool,
        project_id,
        frame_data_urls,
        expectations,
        &video_qc_check_catalog(),
    )
    .await
}

async fn evaluate_image_parts(
    pool: &PgPool,
    project_id: i64,
    image_data_urls: Vec<String>,
    expectations: &str,
    catalog: &[(&str, &str)],
) -> Result<VisualQcReport, String> {
    if image_data_urls.is_empty() {
        return Err("没有可质检的图片".into());
    }
    let mut content = vec![
        json!({"type":"text","text":format!("期望说明：\n{expectations}")}),
    ];
    content.extend(
        image_data_urls
            .iter()
            .map(|url| json!({"type":"image_url","image_url":{"url":url,"detail":"high"}})),
    );
    let messages = vec![
        json!({"role":"system","content":qc_system_prompt_for(catalog)}),
        json!({"role":"user","content":json!(content)}),
    ];
    let value = crate::ai_client::project_text_tools(
        pool,
        VISUAL_QC_AGENT_KEY,
        project_id,
        messages,
        vec![qc_tool_for(catalog)],
    )
    .await?;
    parse_verdict_for(&value, catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_passing_verdict() {
        let report = parse_qc_verdict(&json!({
            "passed": true,
            "summary": "人物、服装与场景均符合",
            "failures": []
        }))
        .unwrap();
        assert!(report.passed);
        assert!(repair_instructions(&report).is_none());
    }

    #[test]
    fn builds_storyboard_expectations_with_optional_framing() {
        let full = storyboard_expectations(
            "沈辞推门进入机房",
            "近景",
            "跟镜",
            "夜",
            "手机（现代）",
            &[("沈辞", "role"), ("机房", "scene")],
        );
        assert!(full.contains("画面描述：沈辞推门进入机房"));
        assert!(full.contains("景别：近景"));
        assert!(full.contains("运镜：跟镜"));
        assert!(full.contains("沈辞（role）、机房（scene）"));
        assert!(full.contains("时间：夜"));
        assert!(full.contains("随身物件（必须出现在画面中且年代特征正确）：手机（现代）"));
        let minimal = storyboard_expectations("空镜扫过桌面", "", "", "", "", &[]);
        assert_eq!(minimal, "画面描述：空镜扫过桌面");
    }

    #[test]
    fn video_catalog_is_enforced_separately_from_images() {
        let verdict = parse_video_qc_verdict(&json!({
            "passed": false, "summary": "首帧不一致",
            "failures": [{"check":"first_frame_similarity","evidence":"首帧人物朝向与参考相反","confidence":0.88}]
        }))
        .unwrap();
        let repair = repair_instructions_for(&verdict, &video_qc_check_catalog()).unwrap();
        assert!(repair.contains("首帧与参考清单中的首帧内容一致"));
        // 图片维度不得混入视频结论。
        assert!(parse_video_qc_verdict(&json!({
            "passed": false, "summary": "",
            "failures": [{"check":"costume","evidence":"不对","confidence":0.9}]
        }))
        .unwrap_err()
        .contains("未知质检维度"));
    }

    #[test]
    fn builds_video_frame_expectations_with_optional_sections() {
        let full = video_frame_expectations(
            "沈辞推门（近景）→ 环视机房（全景）",
            "推镜、跟镜",
            "沈辞站姿全身，黑色雨衣",
        );
        assert!(full.contains("镜头内容："));
        assert!(full.contains("运镜要求：推镜、跟镜"));
        assert!(full.contains("首帧参考：沈辞站姿全身，黑色雨衣"));
        let minimal = video_frame_expectations("空镜扫过桌面", "", "");
        assert_eq!(minimal, "镜头内容：空镜扫过桌面");
    }

    #[test]
    fn parses_failures_and_builds_targeted_repair_instructions() {
        let report = parse_qc_verdict(&json!({
            "passed": false,
            "summary": "多指且服装不对",
            "failures": [
                {"check":"proportion","evidence":"左手六根手指","confidence":0.92},
                {"check":"costume","evidence":"参考图是黑色雨衣，画面是白色T恤","confidence":0.8}
            ]
        }))
        .unwrap();
        assert!(!report.passed);
        assert_eq!(report.failures.len(), 2);
        let repair = repair_instructions(&report).unwrap();
        assert!(repair.contains("人体比例与透视正常"));
        assert!(repair.contains("左手六根手指"));
        assert!(repair.contains("服装与造型描述一致"));
    }

    #[test]
    fn rejects_unknown_checks_and_inconsistent_verdicts() {
        assert!(parse_qc_verdict(&json!({
            "passed": false, "summary": "", "failures": []
        }))
        .is_err());
        assert!(parse_qc_verdict(&json!({
            "passed": true, "summary": "",
            "failures": [{"check":"identity","evidence":"不像","confidence":0.9}]
        }))
        .is_err());
        assert!(parse_qc_verdict(&json!({
            "passed": false, "summary": "",
            "failures": [{"check":"vibe","evidence":"不好看","confidence":0.9}]
        }))
        .unwrap_err()
        .contains("未知质检维度"));
        assert!(parse_qc_verdict(&json!({
            "passed": false, "summary": "",
            "failures": [{"check":"identity","evidence":"不像","confidence":1.5}]
        }))
        .is_err());
    }
}
