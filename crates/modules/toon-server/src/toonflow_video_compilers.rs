//! P0.3 模型专用编译器：按模型家族声明视频能力，在提交供应商前校验
//! 模式与参考图基数。数据库模型能力字段（`videoResolutions` 等）仍然
//! 优先生效；家族声明是防错层——数据库误配置了家族不支持的组合时，
//! 这里明确失败，绝不静默退化成含义不同的模式。

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VideoModelFamily {
    Seedance,
    Wan,
    Seedream,
    Generic,
}

pub(crate) fn family_of(model: &str) -> VideoModelFamily {
    let model = model.to_ascii_lowercase();
    if model.contains("seedance") {
        VideoModelFamily::Seedance
    } else if model.contains("seedream") {
        VideoModelFamily::Seedream
    } else if model.contains("wan") {
        VideoModelFamily::Wan
    } else {
        VideoModelFamily::Generic
    }
}

pub(crate) struct FamilyCapabilities {
    pub(crate) supports_video: bool,
    pub(crate) supports_last_frame: bool,
    pub(crate) max_references: usize,
}

pub(crate) fn family_capabilities(family: VideoModelFamily) -> FamilyCapabilities {
    match family {
        VideoModelFamily::Seedance => FamilyCapabilities {
            supports_video: true,
            supports_last_frame: true,
            max_references: 4,
        },
        // Wan 系列以文生/图生视频为主，参考图只吃首帧。
        VideoModelFamily::Wan => FamilyCapabilities {
            supports_video: true,
            supports_last_frame: false,
            max_references: 1,
        },
        // Seedream 是图像生成家族，视频请求必须显式失败。
        VideoModelFamily::Seedream => FamilyCapabilities {
            supports_video: false,
            supports_last_frame: false,
            max_references: 0,
        },
        VideoModelFamily::Generic => FamilyCapabilities {
            supports_video: true,
            supports_last_frame: true,
            max_references: 4,
        },
    }
}

/// 家族级提交前校验：不支持的组合返回业务错误，调用方必须停止生成。
pub(crate) fn validate_family_request(
    model: &str,
    mode: &str,
    reference_count: usize,
) -> Result<(), String> {
    let capabilities = family_capabilities(family_of(model));
    if !capabilities.supports_video {
        return Err(format!(
            "{model} 属于图像生成模型家族，不能用于视频生成；请选择视频模型"
        ));
    }
    let needs_last_frame = matches!(mode, "startEndRequired" | "endFrameOptional");
    if needs_last_frame && !capabilities.supports_last_frame {
        return Err(format!(
            "{model} 不支持尾帧参考模式（{mode}）；请改用首帧或文本模式，系统不会静默降级"
        ));
    }
    if reference_count > capabilities.max_references {
        return Err(format!(
            "{model} 最多接受 {} 张参考图，实际 {reference_count} 张",
            capabilities.max_references
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_families_from_model_names() {
        assert_eq!(family_of("doubao-seedance-1-5-pro"), VideoModelFamily::Seedance);
        assert_eq!(family_of("Wan2.2-T2V"), VideoModelFamily::Wan);
        assert_eq!(family_of("seedream-4.0"), VideoModelFamily::Seedream);
        assert_eq!(family_of("1784249635985"), VideoModelFamily::Generic);
    }

    #[test]
    fn rejects_video_requests_for_image_families() {
        let error = validate_family_request("seedream-4.0", "text", 0).unwrap_err();
        assert!(error.contains("图像生成模型家族"));
    }

    #[test]
    fn rejects_last_frame_modes_for_first_frame_families() {
        let error =
            validate_family_request("wan2.2-i2v", "startEndRequired", 2).unwrap_err();
        assert!(error.contains("不支持尾帧参考模式"));
        assert!(validate_family_request("wan2.2-i2v", "singleImage", 1).is_ok());
        assert!(validate_family_request("wan2.2-i2v", "text", 1).is_ok());
    }

    #[test]
    fn enforces_family_reference_caps() {
        assert!(validate_family_request("seedance-1-5", "text", 4).is_ok());
        assert!(validate_family_request("seedance-1-5", "text", 5).is_err());
        assert!(validate_family_request("wan2.2", "text", 2).is_err());
    }
}
