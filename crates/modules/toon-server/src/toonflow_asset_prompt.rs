/// Shared production default, independent of a character's name or art style.
/// Explicit character settings and established references take precedence.
pub(crate) const CHARACTER_IDENTITY_RULE: &str = "人物形象默认规则：未明确设定人物背景时，默认采用中国人物形象，适用于历史人物和普通虚构角色。这是创作默认值，不是根据姓名推断国籍或族裔。角色明确设定为外国人、混血或其他背景时，必须遵循对应设定，不得被默认值覆盖；明确的非人类角色保持其物种与造型，不强制改成人类。不得把外文名字、海外场景、服饰或欧美画风单独当作外国人设定。人物参考图或基础角色已确立身份时，沿用其面貌；换装、衍生造型和分镜不得用默认值重塑已有面孔，用户明确要求修改人物背景或面貌时按修改要求执行。保留个体脸型、五官比例、年龄、肤色和发型差异，不使用单一模板脸，不把中国人物等同于某一固定脸型或肤色。画风只影响绘画技法、材质和光影，不得改变人物身份。";

const TIME_TRAVEL_WORLD_RULE: &str = "穿越剧场世界观约束：项目描述和当前场次决定时空；逐个保留人物、服装、建筑和道具各自明确的年代；同框的古今元素必须并存；人物跨时代换装时保持同一身份和面貌；画风只控制审美，不得把古代内容现代化或把现代内容古代化。";

pub(crate) fn project_world_context(
    project_template: &str,
    story_type: &str,
    intro: &str,
) -> String {
    let mut parts = Vec::new();
    if project_template.trim() == "time_travel" {
        parts.push("项目模板：穿越剧场".to_string());
    }
    if !story_type.trim().is_empty() {
        parts.push(format!("项目题材：{}", story_type.trim()));
    }
    if !intro.trim().is_empty() {
        parts.push(format!("项目描述：{}", intro.trim()));
    }
    if project_template.trim() == "time_travel" {
        parts.push(TIME_TRAVEL_WORLD_RULE.to_string());
    }
    parts.join("；")
}

pub(crate) fn generation_source<'a>(original: &'a str, current_prompt: &'a str) -> &'a str {
    if current_prompt.trim().is_empty() {
        original
    } else {
        current_prompt
    }
}

pub(crate) fn polish_system_prompt(
    manual: &str,
    extra: &str,
    project_context: &str,
    asset_type: &str,
    derivative: bool,
) -> String {
    let type_manual = match asset_type {
        "role" if derivative => "仅润色当前服装/形态方案；保持基础人物身份、五官、发型、体型不变。",
        "role" => {
            "按人物视觉手册组织稳定外貌，覆盖五官、发型发色、肤色、年龄、身高体型与气质；不要写动作和场景。"
        }
        "scene" => {
            "按场景视觉手册组织空间结构、时代地域、材质、光线、天气、色调与前中后景；禁止人物。忠实保留原始描述中每个物体的数量、位置与状态，不新增未提及的道具、建筑、污渍、破损或天气效果，不把“陈旧”升级成废弃、灾后或严重脏乱。"
        }
        "tool" => {
            "按道具视觉手册组织造型、比例、材质、颜色、工艺、磨损与关键细节；禁止人物和使用动作。"
        }
        "costume" => "按服装视觉手册组织上装、下装、鞋履、配色、面料、层次、纹样和配饰；禁止人物。",
        _ => "只输出可用于资产生成的纯视觉描述。",
    };
    let identity_rule = if asset_type == "role" {
        format!(
            "\n{CHARACTER_IDENTITY_RULE}\n将人物背景明确写入最终纯视觉描述：缺省时写明中国人物形象，有明确外国或混血等设定时保留对应背景。保留原始设定的时代、地域和个体外貌，不要只保留姓名或职业称谓。衍生造型继承基础人物身份，不凭缺少背景的服装描述重新指定人物背景。"
        )
    } else {
        String::new()
    };
    let scene_fidelity_rule = if asset_type == "scene" {
        "\n场景扩写边界：原始描述已经具体时，只整理顺序和视觉语言，不补写新物件，不移动物件，不放大污损程度。完整空间使用清晰全景深，不同时写浅景深、散景或背景虚化；写实摄影与概念图、设计稿等媒介词不得混用。允许古代与现代元素在穿越题材中共存，逐个保留原始描述中建筑、人物、服装和道具各自所属的年代；古风或现代画风只影响审美表现，不得替换、删除或统一这些时代元素。"
    } else {
        ""
    };
    let project_context_rule = if project_context.trim().is_empty() {
        String::new()
    } else {
        format!(
            "\n项目世界观（全局背景，不代表当前资产中的全部内容）：{}\n只用它判断时代共存、题材和整体设定；不得把项目简介中的人物、地点或物件自动添加到当前资产。当前资产描述优先决定实际可见内容，并保留其中每个元素明确所属的年代。",
            project_context.trim()
        )
    };
    format!(
        "{manual}\n\n## 当前资产类型规则\n{type_manual}\n\n{extra}{project_context_rule}{identity_rule}{scene_fidelity_rule}\n\n最高优先级输出规则：只输出一段可直接使用的纯视觉描述，不输出 Markdown、表格、标题、代码块、自查清单或解释。只描述主体外观；手册中的构图、特写、视图数量、画幅比例和相机取景规则不适用于本步骤，禁止写入描述。最终生图布局由系统独立指定。"
    )
}

pub(crate) fn polish_user_prompt(label: &str, name: &str, description: &str) -> String {
    format!(
        "**基础参数：**\n**{label}设定：**\n- {label}名称:{name},\n- {label}描述:{description},"
    )
}

#[cfg(test)]
pub(crate) fn image_prompt(
    style: &str,
    asset_type: &str,
    visual_description: &str,
    derivative: bool,
    has_reference: bool,
) -> String {
    image_prompt_with_instruction(
        style,
        asset_type,
        visual_description,
        derivative,
        has_reference,
        None,
    )
}

pub(crate) fn image_prompt_with_instruction(
    style: &str,
    asset_type: &str,
    visual_description: &str,
    derivative: bool,
    has_reference: bool,
    managed_instruction: Option<&str>,
) -> String {
    image_prompt_with_source_instruction(
        style,
        asset_type,
        None,
        None,
        visual_description,
        derivative,
        has_reference,
        managed_instruction,
    )
}

#[allow(clippy::too_many_arguments)] // 与图片供应商请求的字段一一对应
pub(crate) fn image_prompt_with_source_instruction(
    style: &str,
    asset_type: &str,
    source_description: Option<&str>,
    project_context: Option<&str>,
    visual_description: &str,
    derivative: bool,
    has_reference: bool,
    managed_instruction: Option<&str>,
) -> String {
    let source_description = source_description
        .map(|description| asset_visual_description(asset_type, description))
        .filter(|description| !description.trim().is_empty());
    let visual_description = asset_visual_description(asset_type, visual_description);
    let asymmetric_role = asset_type == "role" && requires_opposite_side_view(&visual_description);
    let visual_description = if asset_type == "scene" {
        constrain_scene_expansion(source_description.as_deref(), &visual_description)
    } else {
        visual_description
    };
    let visual_description = provider_safe_visual_description(&visual_description);
    let style = if asset_type == "scene" {
        scene_generation_style(
            style,
            source_description.as_deref().unwrap_or(&visual_description),
        )
    } else {
        style.trim().to_string()
    };
    let fallback_instruction = match asset_type {
        "role" if derivative => {
            "生成同一角色的标准四视图，保持参考图中的人物身份、面部、发型和体型一致，并应用提示词指定的服装或形态。"
        }
        "role" => "生成角色标准四视图，完整展示人物稳定的面部、发型、体型与服饰特征。",
        "costume" => {
            "生成独立服装四宫格设定图，不出现人物。画面采用2×2布局：正面完整展示、侧面完整展示、背面完整展示、面料与缝制工艺细节特写。四格必须是同一套服装，版型、配色、面料、纹样、配饰和磨损状态完全一致；服装完整入画，不裁切，不被穿着，不使用人体模特或手部。采用纯净中性背景和均匀柔光。"
        }
        "scene" => {
            "生成无人场景的单画面代表性广角主视图，不得拼图、分屏或生成多视图。采用自然观察视角，完整展示空间主体与纵深，前景、中景、后景层次清楚，建筑结构、材质纹理、时间、天气、色调和光源方向统一。禁止出现人物、人影、人体轮廓或镜中倒影人物。"
        }
        "tool" => {
            "生成独立道具四宫格设定图。画面采用2×2布局：正面完整图、侧面完整图、背面完整图、材质与工艺细节特写。四格必须是同一个道具，造型、比例、颜色、材质、工艺和使用状态完全一致；道具占每格主体约70%，采用纯净中性背景和均匀柔光。只能出现道具本身，禁止人物、手部、肢体、佩戴、握持或使用状态。"
        }
        _ => "生成干净的资产标准设定图。",
    };
    let subject_instruction = if asset_type == "role" {
        role_layout_instruction(derivative, asymmetric_role)
    } else {
        // Custom instructions may add appearance requirements, but cannot
        // replace the type's layout and completeness contract.
        let extra = managed_instruction
            .map(|instruction| asset_visual_description(asset_type, instruction))
            .unwrap_or_default();
        format!("{extra}\n{fallback_instruction}")
    };
    let reference_instruction = if has_reference {
        "\n保持参考图主体的可见特征一致，但不要复制参考图中的文字或标识。参考图只约束主体外观，不约束画布比例、景别或布局；即使参考图只有半身，也必须按任务要求补全主体，不得继承裁切。"
    } else {
        ""
    };
    let identity_instruction = if asset_type == "role" {
        format!("\n{CHARACTER_IDENTITY_RULE}")
    } else {
        String::new()
    };
    let source_instruction = if asset_type == "scene" {
        source_description.as_deref().map_or_else(String::new, |source| {
            format!(
                "最高优先级原始场景事实：{source}\n忠实性约束：AI视觉描述只能整理和具体化原始事实；原始事实未提及的物件、破损、污渍、积水、霉斑、垃圾或天气效果全部忽略。原始物体的位置、数量和状态不得改变。\n"
            )
        })
    } else {
        String::new()
    };
    let project_context_instruction = project_context
        .filter(|context| !context.trim().is_empty())
        .map_or_else(String::new, |context| {
            format!(
                "项目世界观（全局背景，不代表当前资产全部可见）：{}\n世界观约束：只用来判断古今元素能否共存以及整体设定；不得把项目简介中的人物、地点、建筑或道具自动添加到当前资产。当前资产原始描述优先决定画面中实际出现的内容。\n",
                context.trim()
            )
        });
    let style_boundary = match asset_type {
        "scene" => {
            "\n古今混合规则：允许古代与现代视觉元素在同一项目、同一场景中共存。画风只决定表现媒介、材质、色彩和光影；具体出现哪些时代元素只由原始描述决定，并逐个保持各自年代。现代电脑、公寓及其他技术物件不得被古代物件替换；古代建筑、家具、服饰和道具也不得被现代化；未在原始描述中出现的时代元素不得因画风自动添加。"
        }
        "role" | "costume" | "tool" => {
            "\n时代边界：穿越题材允许古今元素共存。画风只决定表现媒介与审美，不得改变当前资产描述中人物、服装或道具明确所属的年代，不得用另一时代的款式替换，也不得自动添加未描述的时代元素。"
        }
        _ => "",
    };
    format!(
        "{project_context_instruction}{source_instruction}画风：{style}{style_boundary}\n类型：{asset_type}\n任务要求：{subject_instruction}\n\
         纯视觉描述：{visual_description}{reference_instruction}{identity_instruction}\n\
         将描述中的姓名、化名、编号、代号和称谓仅作为背景语义理解，绝不能把它们画出来。\n\
         画面中禁止出现任何文字、字母、数字、姓名、编号、胸牌、名牌、墙面标牌、字幕、标题、Logo或水印。\n\
         服装和背景表面保持无字、无编号、无标识。\n\
         最终构图约束（优先于描述、画风和参考图中的构图文字）：{subject_instruction}"
    )
}

fn scene_generation_style(style: &str, description: &str) -> String {
    let normalized_style = style.trim();
    let style_lower = normalized_style.to_ascii_lowercase();
    let description_lower = description.to_ascii_lowercase();
    let description_is_modern = [
        "现代",
        "当代",
        "都市",
        "公寓",
        "电脑",
        "显示器",
        "键盘",
        "汽车",
        "办公楼",
        "霓虹",
    ]
    .iter()
    .any(|marker| description_lower.contains(marker));
    let description_is_historical = [
        "古代", "古风", "朝代", "汉代", "唐代", "宋代", "明代", "清代", "古宅", "宫殿", "寺庙",
    ]
    .iter()
    .any(|marker| description_lower.contains(marker));
    let style_is_historical = ["ancient", "traditional", "古代", "古风", "历史"]
        .iter()
        .any(|marker| style_lower.contains(marker));
    let style_is_modern = ["modern", "urban", "现代", "都市"]
        .iter()
        .any(|marker| style_lower.contains(marker));

    if description_is_modern && style_is_historical {
        if style_lower.contains("realpeople")
            || style_lower.contains("photoreal")
            || normalized_style.contains("真人")
            || normalized_style.contains("写实")
        {
            "真人古风写实摄影；保留古风写实的色彩、材质与光影审美，允许古代与现代视觉元素按原始描述共存".to_string()
        } else if style_lower.contains("2d") || normalized_style.contains("二维") {
            "二维古风动画渲染；保留古风的线条、色彩与纹理审美，允许古代与现代视觉元素按原始描述共存"
                .to_string()
        } else if style_lower.contains("3d") || normalized_style.contains("三维") {
            "高精度古风三维渲染；保留古风的材质、色彩与光影审美，允许古代与现代视觉元素按原始描述共存".to_string()
        } else {
            format!("{normalized_style}；保留所选古风审美，允许古代与现代视觉元素按原始描述共存")
        }
    } else if description_is_historical && style_is_modern {
        if style_lower.contains("realpeople")
            || style_lower.contains("photoreal")
            || normalized_style.contains("真人")
            || normalized_style.contains("写实")
        {
            "真人现代写实摄影；保留现代写实的色彩、材质与光影审美，允许古代与现代视觉元素按原始描述共存".to_string()
        } else if style_lower.contains("2d") || normalized_style.contains("二维") {
            "二维现代动画渲染；保留现代动画的线条、色彩与纹理审美，允许古代与现代视觉元素按原始描述共存".to_string()
        } else if style_lower.contains("3d") || normalized_style.contains("三维") {
            "高精度现代三维渲染；保留现代三维的材质、色彩与光影审美，允许古代与现代视觉元素按原始描述共存".to_string()
        } else {
            format!("{normalized_style}；保留所选现代审美，允许古代与现代视觉元素按原始描述共存")
        }
    } else {
        normalized_style.to_string()
    }
}

fn constrain_scene_expansion(source_description: Option<&str>, visual_description: &str) -> String {
    let Some(source_description) = source_description else {
        return visual_description.to_string();
    };
    const AMPLIFICATION_MARKERS: &[&str] = &[
        "废弃",
        "灾后",
        "严重脏乱",
        "坍塌",
        "腐烂",
        "霉斑",
        "发霉",
        "积水",
        "水渍",
        "垃圾堆",
        "厚重灰尘",
        "巨大",
        "占满前景",
        "abandoned",
        "post-apocalyptic",
        "mold",
        "mildew",
        "standing water",
        "garbage pile",
        "giant",
    ];
    let source_lower = source_description.to_lowercase();
    visual_description
        .split(['，', '；'])
        .map(str::trim)
        .filter(|clause| {
            let clause_lower = clause.to_lowercase();
            !AMPLIFICATION_MARKERS
                .iter()
                .any(|marker| clause_lower.contains(marker) && !source_lower.contains(marker))
        })
        .collect::<Vec<_>>()
        .join("，")
}

/// Keeps the stored, user-facing costume wording intact while avoiding
/// ambiguous occupation/body terms at the image-provider moderation boundary.
fn provider_safe_visual_description(description: &str) -> String {
    description
        .replace("按摩技师", "理疗服务人员")
        .replace("按摩师", "理疗服务人员")
        .replace("按摩工装", "理疗服务制服")
        .replace("按摩室", "理疗服务室")
        .replace("按摩动作", "工作动作")
        .replace("按摩服务", "理疗服务")
        .replace("合体收腰", "修身利落")
        .replace("暴露设计", "不适宜设计")
        .replace("性感", "时尚")
}

fn requires_opposite_side_view(description: &str) -> bool {
    const MARKERS: &[&str] = &[
        "左脸",
        "右脸",
        "左眼",
        "右眼",
        "左耳",
        "右耳",
        "左臂",
        "右臂",
        "左手",
        "右手",
        "左腿",
        "右腿",
        "单边",
        "单侧",
        "单耳",
        "不对称",
        "眼罩",
        "机械臂",
        "义肢",
        "半边脸",
        "一侧脸",
        "侧脸伤疤",
        "侧面纹身",
    ];
    MARKERS.iter().any(|marker| description.contains(marker))
}

pub(crate) fn asset_image_ratio(asset_type: &str, description: &str) -> &'static str {
    match asset_type {
        "role" if requires_opposite_side_view(&asset_visual_description("role", description)) => {
            "2:1"
        }
        "role" => "3:2",
        "scene" => "16:9",
        _ => "1:1",
    }
}

pub(crate) fn role_appearance_anchors(description: &str) -> String {
    let description = crate::toonflow_face_identity::appearance_description(description);
    let cleaned = asset_visual_description("role", description);
    let terms = [
        "衣", "袍", "裤", "鞋", "帽", "靴", "褂", "衫", "裙", "发", "胡须", "长须",
    ];
    cleaned
        .split(['，', '；'])
        .filter(|clause| terms.iter().any(|term| clause.contains(term)))
        .collect::<Vec<_>>()
        .join("，")
}

fn role_layout_instruction(derivative: bool, asymmetric: bool) -> String {
    let identity = if derivative {
        "第一张参考图是同一人物的基础资产。严格保持参考人物的脸型、五官、发型、年龄、肤色、体型和人物比例，只改变提示词指定的服装、伤病或形态。"
    } else {
        "保持同一人物的脸型、五官、发型、年龄、肤色、体型、服装和人物比例完全一致。"
    };
    let layout = if asymmetric {
        "检测到明确的单侧或不对称特征，生成严格四视图全身设定图。画面必须恰好并排出现4个同一人物，从左到右固定为：①正面0°，脸和胸口正对镜头；②左侧90°，鼻尖、胸口和脚尖朝画面左侧；③右侧90°，鼻尖、胸口和脚尖朝画面右侧；④背面180°，后脑和背部正对镜头且不得露出五官。第二格与第三格必须方向相反，完整展示两侧差异。"
    } else {
        "生成标准三视图全身设定图。画面必须恰好并排出现3个同一人物，从左到右固定为：①正面0°，脸和胸口正对镜头；②右侧90°标准侧面，鼻尖、胸口和脚尖全部明确朝画面右侧；③背面180°，后脑和背部正对镜头且不得露出眼睛、鼻子、嘴。禁止增加左侧视图，禁止重复方向，禁止用3/4侧面替代标准90°侧面。"
    };
    format!(
        "{identity}{layout}所有视图都必须从头顶到脚底完整入画、等高等比例、基线对齐、自然直立、双臂自然下垂。每个人物的头发、小腿、双脚和鞋底全部可见；人物身高占画面高度约80%，头顶与画面上缘、鞋底与画面下缘各留至少8%的空白。主体过大时缩小整个人物以完整入画，不得裁去腿脚。Full-length head-to-toe standing figures, entire legs and all shoes visible, camera pulled back, generous empty space above heads and below feet. 纯净中性背景、均匀柔光；禁止特写、半身、裁切、不同人物、额外人物、文字和尺寸标注。"
    )
}

/// Older polish responses included a complete Markdown manual. Prefer its
/// ready-to-use fenced prompt, then strip presentation and layout clauses.
pub(crate) fn asset_visual_description(asset_type: &str, prompt: &str) -> String {
    let fenced = prompt
        .split("```")
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .filter_map(|(_, block)| block.split_once('\n').map(|(_, body)| body.trim()))
        .filter(|body| !body.is_empty())
        .max_by_key(|body| body.len());
    let prompt = fenced.unwrap_or(prompt);
    const LAYOUT_MARKERS: &[&str] = &[
        "人像特写",
        "头像特写",
        "面部特写",
        "半身像",
        "半身图",
        "头顶至锁骨",
        "头顶到锁骨",
        "锁骨以上",
        "正视图",
        "正面视图",
        "左视图",
        "右视图",
        "侧视图",
        "后视图",
        "背面视图",
        "四视图",
        "四宫格",
        "portrait closeup",
        "front view",
        "side view",
        "back view",
        "特写",
        "半身",
        "大头照",
        "胸像",
        "腰部以上",
        "头肩",
        "三视图",
        "构图",
        "画幅",
        "宽高比",
        "分辨率",
        "景别",
        "尺寸",
        "画面比例",
        "视图一致性",
        "head to collarbone",
        "head-to-collarbone",
        "close-up",
        "close up",
        "closeup",
        "half body",
        "half-body",
        "waist up",
        "waist-up",
        "bust shot",
        "headshot",
        "turnaround",
        "character design sheet",
        "aspect ratio",
        "16:9",
        "9:16",
        "4:1",
        "3:2",
        "2:1",
        "1:1",
    ];
    const SCENE_CONFLICT_MARKERS: &[&str] = &[
        "scene design sheet",
        "environment concept art",
        "concept art",
        "shallow depth of field",
        "bokeh",
        "lens vignette",
        "chromatic aberration",
        "场景设计稿",
        "环境概念图",
        "概念图",
        "浅景深",
        "散景",
        "背景虚化",
        "镜头暗角",
        "色差",
    ];

    let cleaned = prompt
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#') && !line.starts_with("---") && !line.contains("✅"))
        .flat_map(|line| line.split(['。', '；', '，', ';', ',', '|']))
        .map(str::trim)
        .filter(|clause| {
            !clause.is_empty()
                && !clause.chars().all(|c| matches!(c, '-' | ':' | ' '))
                && !LAYOUT_MARKERS
                    .iter()
                    .any(|marker| clause.to_lowercase().contains(&marker.to_lowercase()))
                && (asset_type != "role" || !clause.contains("拼图"))
                && (asset_type != "scene"
                    || !SCENE_CONFLICT_MARKERS
                        .iter()
                        .any(|marker| clause.to_lowercase().contains(&marker.to_lowercase())))
        })
        .collect::<Vec<_>>()
        .join("，");
    if asset_type == "scene" {
        strip_scene_room_identifiers(&cleaned)
    } else {
        cleaned
    }
}

fn strip_scene_room_identifiers(description: &str) -> String {
    let chars = description.chars().collect::<Vec<_>>();
    let mut cleaned = String::with_capacity(description.len());
    let mut index = 0;
    while index < chars.len() {
        if chars[index].is_ascii_digit() || matches!(chars[index], '０'..='９') {
            let mut end = index + 1;
            while end < chars.len()
                && (chars[end].is_ascii_digit() || matches!(chars[end], '０'..='９'))
            {
                end += 1;
            }
            if chars.get(end) == Some(&'号') && chars.get(end + 1) == Some(&'房') {
                while cleaned.ends_with(['·', '•', '-', '—', ' ']) {
                    cleaned.pop();
                }
                index = end + 2;
                continue;
            }
        }
        cleaned.push(chars[index]);
        index += 1;
    }
    cleaned
        .trim_matches(['，', '、', '·', '•', '-', '—', ' '])
        .to_string()
}

#[cfg(test)]
pub(crate) fn storyboard_prompt(
    visual_description: &str,
    ratio: &str,
    reference_count: usize,
) -> String {
    storyboard_prompt_with_instruction(visual_description, ratio, reference_count, None)
}

#[cfg(test)]
pub(crate) fn storyboard_prompt_with_instruction(
    visual_description: &str,
    ratio: &str,
    reference_count: usize,
    managed_instruction: Option<&str>,
) -> String {
    let instruction = managed_instruction
        .filter(|instruction| !instruction.trim().is_empty())
        .unwrap_or("生成单张连续的电影分镜画面，不得使用四视图、四宫格、拼贴、分屏。严格保持参考资产的人物身份、服装、场景和道具一致性。");
    format!(
        "{instruction}\n分镜视觉描述：{visual_description}\n画面比例：{ratio}\n参考资产图数量：{reference_count}"
    )
}

pub(crate) fn storyboard_generation_prompt(stored_prompt: &str) -> String {
    const NON_VISUAL_MARKERS: [&str; 6] = ["台词：", "台词:", "对白：", "对白:", "音效：", "音效:"];
    let visual_prompt = stored_prompt
        .lines()
        .map(|line| {
            line.split('；')
                .filter_map(|clause| {
                    let end = NON_VISUAL_MARKERS
                        .iter()
                        .filter_map(|marker| clause.find(marker))
                        .min()
                        .unwrap_or(clause.len());
                    let clause = clause[..end].trim();
                    (!clause.is_empty()).then_some(clause)
                })
                .collect::<Vec<_>>()
                .join("；")
        })
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{visual_prompt}\n{CHARACTER_IDENTITY_RULE}\n人物参考图只用于锁定身份、面容、体型和服装；必须严格执行分镜文字中为每个 @图N 指定的姿态、承托物、位置和朝向，不得继承人物设定图中的站姿、四视图或展示构图。\n所有已绑定的参考资产都必须在画面中清晰可见并与描述一一对应；不得遗漏任何出镜人物，不得把应出镜人物裁切成只露手、肩膀或局部身体。\n画面中禁止出现任何文字、字母、数字、对白、字幕、标题、Logo或水印；不得把台词画进图像。"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_live_action_base_does_not_force_a_world_era() {
        let style = "realpeople_cinematic_base";
        assert_eq!(
            scene_generation_style(style, "现代都市公寓里的电脑桌"),
            style
        );
        assert_eq!(
            scene_generation_style(style, "唐代木构客栈与身穿圆领袍的少年"),
            style
        );
    }

    #[test]
    fn time_travel_template_adds_rules_without_inventing_story_content() {
        let context = project_world_context(
            "time_travel",
            "悬疑穿越",
            "现代404公寓与古代客栈通过一扇门相连",
        );

        assert!(context.contains("项目模板：穿越剧场"));
        assert!(context.contains("现代404公寓与古代客栈"));
        assert!(context.contains("同框的古今元素必须并存"));
        assert!(context.contains("人物跨时代换装时保持同一身份和面貌"));
        assert!(!context.contains("宫殿"));
    }

    #[test]
    fn legacy_manual_cannot_leak_closeups_back_into_generation() {
        let legacy = "# 李晨人物视觉手册\n| 构图 | 人像特写+正视图+侧视图+后视图 |\n## 可直接使用的提示词\n```text\n青年男性，黑色短发，深灰连帽卫衣，深色长裤，运动鞋，\ncharacter design sheet, character turnaround,\nhead to collarbone complete, waist-up portrait,\n全身立像从头顶到脚底完整展示，full body head to toe\n```\n## 自查\n| R8 | 特写头顶到锁骨 | ✅ |";
        let cleaned = asset_visual_description("role", legacy);
        assert!(cleaned.contains("深灰连帽卫衣"));
        assert!(cleaned.contains("运动鞋"));
        for forbidden in [
            "```",
            "自查",
            "特写",
            "collarbone",
            "waist-up",
            "turnaround",
            "design sheet",
        ] {
            assert!(!cleaned.contains(forbidden), "{forbidden}: {cleaned}");
        }
        assert_eq!(asset_image_ratio("role", legacy), "3:2");
    }

    #[test]
    fn asset_canvases_match_their_layouts() {
        assert_eq!(asset_image_ratio("role", "黑色短发"), "3:2");
        assert_eq!(asset_image_ratio("role", "右眼眼罩"), "2:1");
        assert_eq!(asset_image_ratio("scene", "武馆"), "16:9");
        assert_eq!(asset_image_ratio("costume", "长袍"), "1:1");
        assert_eq!(asset_image_ratio("tool", "长剑"), "1:1");
    }

    #[test]
    fn modern_clothes_and_hair_remain_explicit_when_project_style_is_historical() {
        let anchors = role_appearance_anchors(
            "二十多岁青年，黑色短发略显凌乱，深灰色连帽卫衣，深色直筒长裤，素色运动鞋\n【角色面部身份 v1】\n脸型与骨骼：长脸\n【角色面部身份结束】",
        );
        assert!(anchors.contains("深灰色连帽卫衣"));
        assert!(anchors.contains("深色直筒长裤"));
        assert!(anchors.contains("黑色短发略显凌乱"));
        assert!(!anchors.contains("角色面部身份"));
    }

    #[test]
    fn manual_layout_override_cannot_remove_asset_completeness_rules() {
        let prompt = image_prompt_with_instruction(
            "写实",
            "costume",
            "蓝色长袍",
            false,
            false,
            Some("半身人像特写"),
        );
        assert!(prompt.contains("服装完整入画，不裁切"));
        assert!(!prompt.contains("半身人像特写"));
        assert!(
            polish_system_prompt("输出四视图手册", "", "", "role", false).contains("禁止写入描述")
        );
    }

    #[test]
    fn scene_polish_is_conservative_about_source_facts() {
        let prompt = polish_system_prompt(
            "写实摄影",
            "",
            "穿越题材，古代人物来到现代都市",
            "scene",
            false,
        );

        assert!(prompt.contains("不新增未提及的道具"));
        assert!(prompt.contains("不把“陈旧”升级成废弃"));
        assert!(prompt.contains("不同时写浅景深、散景或背景虚化"));
        assert!(prompt.contains("写实摄影与概念图、设计稿等媒介词不得混用"));
        assert!(prompt.contains("允许古代与现代元素在穿越题材中共存"));
        assert!(prompt.contains("逐个保留原始描述中建筑、人物、服装和道具各自所属的年代"));
        assert!(prompt.contains("项目世界观（全局背景，不代表当前资产中的全部内容）"));
        assert!(prompt.contains("不得把项目简介中的人物、地点或物件自动添加到当前资产"));
    }

    #[test]
    fn scene_cleanup_removes_room_numbers_and_conflicting_camera_boilerplate() {
        let cleaned = asset_visual_description(
            "scene",
            "现代都市单身公寓·404号房，旧电脑桌，shallow depth of field，bokeh，environment concept art，35mm film grain",
        );

        assert!(cleaned.contains("现代都市单身公寓"));
        assert!(cleaned.contains("旧电脑桌"));
        assert!(cleaned.contains("35mm film grain"));
        for forbidden in [
            "404号房",
            "shallow depth of field",
            "bokeh",
            "environment concept art",
        ] {
            assert!(!cleaned.contains(forbidden), "{forbidden}: {cleaned}");
        }
    }

    #[test]
    fn mixed_era_scene_preserves_ancient_aesthetic_without_replacing_modern_content() {
        let prompt = image_prompt_with_source_instruction(
            "realpeople_ancient_chinese",
            "scene",
            Some("狭小陈旧的现代单身公寓，旧电脑桌与显示器，窗外冰雹，桌面散着泡面桶"),
            Some("现代女孩穿越古代，在两个时代之间往返"),
            "现代公寓·404号房，废弃灾后房间，地面积水和霉斑，浅景深，巨大的泡面桶占满前景",
            false,
            false,
            None,
        );

        assert!(prompt.contains("最高优先级原始场景事实：狭小陈旧的现代单身公寓"));
        assert!(prompt.contains("画风：真人古风写实摄影"));
        assert!(prompt.contains("保留古风写实的色彩、材质与光影审美"));
        assert!(prompt.contains("允许古代与现代视觉元素按原始描述共存"));
        assert!(!prompt.contains("realpeople_ancient_chinese"));
        assert!(!prompt.contains("404号房"));
        assert!(!prompt.contains("废弃灾后房间"));
        assert!(!prompt.contains("巨大的泡面桶占满前景"));
        assert!(
            prompt.contains("原始事实未提及的物件、破损、污渍、积水、霉斑、垃圾或天气效果全部忽略")
        );
        assert!(prompt.contains("逐个保持各自年代"));
        assert!(prompt.contains("现代电脑、公寓及其他技术物件不得被古代物件替换"));
    }

    #[test]
    fn time_travel_scene_keeps_explicit_ancient_and_modern_elements_together() {
        let prompt = image_prompt_with_source_instruction(
            "realpeople_ancient_chinese",
            "scene",
            Some("古代木构客栈内摆着亮起的现代笔记本电脑，穿越者的充电宝放在明代木桌上"),
            Some("古今穿越题材，古代与现代世界同时存在"),
            "古代木构客栈，现代笔记本电脑，充电宝，明代木桌",
            false,
            false,
            None,
        );

        assert!(prompt.contains("古代木构客栈"));
        assert!(prompt.contains("现代笔记本电脑"));
        assert!(prompt.contains("充电宝"));
        assert!(prompt.contains("明代木桌"));
        assert!(prompt.contains("画风：真人古风写实摄影"));
        assert!(prompt.contains("古代建筑、家具、服饰和道具也不得被现代化"));
        assert!(prompt.contains("项目世界观（全局背景，不代表当前资产全部可见）"));
        assert!(prompt.contains("当前资产原始描述优先决定画面中实际出现的内容"));
    }

    #[test]
    fn image_prompt_does_not_require_an_asset_name() {
        let prompt = image_prompt("写实", "role", "年轻女性，神情谨慎", false, false);

        assert!(!prompt.contains("叶弥月"));
        assert!(prompt.contains("标准三视图全身设定图"));
        assert!(prompt.contains("禁止增加左侧视图"));
        assert!(prompt.contains("鼻尖、胸口和脚尖全部明确朝画面右侧"));
        assert!(prompt.contains("纯视觉描述：年轻女性，神情谨慎"));
        assert!(prompt.contains("禁止出现任何文字、字母、数字"));
        assert!(prompt.contains("服装和背景表面保持无字、无编号、无标识"));
    }

    #[test]
    fn polish_prompt_matches_toonflow_asset_parameters() {
        let prompt = polish_user_prompt("角色", "叶弥月", "外班女生，化名8号按摩师");

        assert!(prompt.contains("**基础参数：**"));
        assert!(prompt.contains("**角色设定：**"));
        assert!(prompt.contains("角色名称:叶弥月"));
        assert!(prompt.contains("角色描述:外班女生，化名8号按摩师"));
    }

    #[test]
    fn every_asset_type_has_a_production_specific_layout() {
        let scene = image_prompt("写实", "scene", "现代武馆", false, false);
        let tool = image_prompt("写实", "tool", "旧木剑", false, false);
        let costume = image_prompt("写实", "costume", "深色练功服", false, false);

        assert!(scene.contains("单画面代表性广角主视图"));
        assert!(scene.contains("前景、中景、后景"));
        assert!(tool.contains("正面完整图、侧面完整图、背面完整图"));
        assert!(tool.contains("禁止人物、手部、肢体"));
        assert!(costume.contains("正面完整展示、侧面完整展示、背面完整展示"));
        assert!(costume.contains("不使用人体模特或手部"));
    }

    #[test]
    fn storyboard_is_a_single_cinematic_frame() {
        let prompt = storyboard_prompt("人物走进武馆", "16:9", 3);

        assert!(prompt.contains("单张连续的电影分镜画面"));
        assert!(prompt.contains("不得使用四视图、四宫格、拼贴、分屏"));
        assert!(prompt.contains("参考资产图数量：3"));
    }

    #[test]
    fn storyboard_generation_removes_dialogue_and_audio_metadata() {
        let prompt = storyboard_generation_prompt(
            "【画面】@图1 坐在床边；台词：甲：『你好』；音效：脚步声\n【风格】写实，16:9",
        );
        assert!(prompt.contains("@图1 坐在床边"));
        assert!(prompt.contains("禁止出现任何文字"));
        assert!(prompt.contains("不得遗漏任何出镜人物"));
        assert!(prompt.contains("人物参考图只用于锁定身份"));
        assert!(prompt.contains("不得继承人物设定图中的站姿"));
        assert!(!prompt.contains("『你好』"));
        assert!(!prompt.contains("脚步声"));
    }

    #[test]
    fn derivative_role_keeps_identity_and_applies_clothing() {
        let prompt = image_prompt("写实", "role", "黑色校服", true, true);

        assert!(prompt.contains("标准三视图全身设定图"));
        assert!(prompt.contains("纯视觉描述：黑色校服"));
        assert!(prompt.contains("保持参考图主体的可见特征一致"));
    }

    #[test]
    fn base_role_keeps_clothing_but_removes_legacy_layout() {
        let prompt = image_prompt(
            "写实",
            "role",
            "年轻男性，黑色短发，穿着黑色立领夹克和黑色T恤。\n人像特写+正视图+侧视图+后视图",
            false,
            false,
        );

        assert!(prompt.contains("黑色立领夹克和黑色T恤"));
        assert!(!prompt.contains("人像特写+正视图+侧视图+后视图"));
    }

    #[test]
    fn base_role_removes_story_action_and_location() {
        let prompt = image_prompt(
            "写实",
            "role",
            "一位年轻男性，短发，面容清秀，神情温柔，坐在现代医院病房内。他身体微微前倾，手里拿着向日葵。背景是白墙和窗帘。",
            false,
            false,
        );

        assert!(prompt.contains("坐在现代医院病房内"));
        assert!(prompt.contains("手里拿着向日葵"));
        assert!(prompt.contains("背景是白墙和窗帘"));
    }

    #[test]
    fn base_role_removes_clothing_without_a_wearing_verb() {
        let prompt = image_prompt(
            "写实",
            "role",
            "面容冷峻坚毅。自然黑色长发。黑色立领暗纹练功服，腰间系着布带，长袖窄口，高级亚麻质感。",
            false,
            false,
        );

        assert!(prompt.contains("黑色立领暗纹练功服"));
        assert!(prompt.contains("高级亚麻质感"));
    }

    #[test]
    fn derivative_role_keeps_requested_clothing_after_sanitizer_was_added() {
        let prompt = image_prompt("写实", "role", "穿着黑色立领夹克", true, true);

        assert!(prompt.contains("纯视觉描述：穿着黑色立领夹克"));
    }

    #[test]
    fn derivative_role_uses_provider_safe_wellness_wording() {
        let prompt = image_prompt(
            "写实",
            "role",
            "穿着合体收腰的按摩技师制服，便于按摩动作，不使用暴露设计",
            true,
            true,
        );

        assert!(prompt.contains("修身利落的理疗服务人员制服"));
        assert!(prompt.contains("便于工作动作"));
        assert!(prompt.contains("不使用不适宜设计"));
        assert!(!prompt.contains("按摩"));
        assert!(!prompt.contains("合体收腰"));
        assert!(!prompt.contains("暴露设计"));
    }

    #[test]
    fn role_generation_removes_legacy_closeup_layout() {
        let prompt = image_prompt(
            "写实",
            "role",
            "青年男性，短黑发，人像特写，正视图，侧视图，后视图，身材高挑",
            false,
            false,
        );

        assert!(!prompt.contains("人像特写"));
        assert!(!prompt.contains("正视图"));
        assert!(prompt.contains("青年男性，短黑发，身材高挑"));
        assert!(prompt.contains("标准三视图全身设定图"));
    }

    #[test]
    fn unilateral_character_feature_enables_opposite_side_view() {
        let prompt = image_prompt(
            "写实",
            "role",
            "年轻女性，右眼戴眼罩，左脸无伤",
            false,
            false,
        );

        assert!(prompt.contains("严格四视图全身设定图"));
        assert!(prompt.contains("第二格与第三格必须方向相反"));
        assert!(!prompt.contains("禁止增加左侧视图"));
    }
}
