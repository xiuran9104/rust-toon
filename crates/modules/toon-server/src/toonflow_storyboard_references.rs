use std::collections::{HashMap, HashSet};

use rust_toon_framework_web::AppError;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};

use crate::toonflow_asset_context::StoryboardAssetReference;

#[derive(Debug)]
pub(crate) struct StoryboardReferencePlan {
    pub(crate) paths: Vec<String>,
    pub(crate) managed_instruction: Option<String>,
    pub(crate) scene_state_id: Option<i64>,
    pub(crate) generation_context: Value,
}

impl StoryboardReferencePlan {
    pub(crate) fn apply_to_prompt(&self, prompt: String) -> String {
        match self.managed_instruction.as_deref() {
            Some(instruction) => format!("{prompt}\n\n{instruction}"),
            None => prompt,
        }
    }
}

#[derive(Debug, FromRow)]
struct StoryboardBinding {
    scene_key: Option<String>,
    scene_state_id: Option<i64>,
}

#[derive(Debug, FromRow)]
struct SceneBinding {
    master_id: i64,
    master_name: String,
    master_asset_id: Option<i64>,
    master_image_id: Option<i64>,
    master_image_path: Option<String>,
    master_status: String,
    master_revision: i32,
    spatial_prompt: String,
    layout_spec: Value,
    state_id: i64,
    state_key: String,
    state_name: String,
    state_revision: i32,
}

#[derive(Clone, Debug, FromRow)]
struct StateConstraint {
    id: i64,
    parent_state_id: Option<i64>,
    state_key: String,
    name: String,
    change_summary: String,
    state_prompt: String,
    object_states: Value,
}

#[derive(Debug, FromRow)]
struct StateReference {
    sort_order: i32,
    role: String,
    asset_id: i64,
    image_id: i64,
    file_path: Option<String>,
    image_state: Option<String>,
    prompt_label: String,
}

#[derive(Debug, Clone)]
struct ManagedReference {
    role: &'static str,
    marker: usize,
    asset_id: Option<i64>,
    image_id: i64,
    label: String,
}

/// 对齐方案 P1 同造型跨场复用：场次键归一化匹配。“sc1”“场1”“第1场”
/// 视为同一场次；造型的 scenes 为空表示适用于整个剧本，视为匹配。
fn scene_matches(scene_key: &str, appearance_scenes: &[String]) -> bool {
    fn normalize(value: &str) -> String {
        let digits: String = value.chars().filter(char::is_ascii_digit).collect();
        if digits.is_empty() {
            value.trim().to_lowercase()
        } else {
            digits
        }
    }
    if appearance_scenes.is_empty() {
        return true;
    }
    let normalized_key = normalize(scene_key);
    appearance_scenes
        .iter()
        .any(|scene| normalize(scene) == normalized_key)
}

/// 分镜显式绑定的人物基础资产，若该场次存在已完成的造型衍生图，则把
/// 参考图替换为造型图——同一服装跨场复用同一张图、不重复生成，也避免
/// 底模基础图直接出镜。返回替换记录供生成上下文留痕。
async fn apply_costume_overrides(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    scene_key: &str,
    explicit: &mut [StoryboardAssetReference],
) -> Vec<Value> {
    let role_asset_ids: Vec<i64> = explicit
        .iter()
        .filter(|reference| matches!(reference.prompt_asset.kind.as_str(), "role" | "character"))
        .map(|reference| reference.asset_id)
        .collect();
    if role_asset_ids.is_empty() {
        return Vec::new();
    }
    let rows: Vec<(i64, i64, String, Value)> = sqlx::query_as(
        r#"SELECT d.parent_asset_id,d.id,i.file_path,ca.scenes
           FROM toonflow.assets d
           JOIN toonflow.character_appearances ca ON ca.id=d.appearance_id
           JOIN toonflow.images i ON i.id=d.image_id
           WHERE d.project_id=$1 AND ca.script_id=$2
             AND d.parent_asset_id=ANY($3)
             AND i.state='已完成' AND coalesce(i.file_path,'')<>''
           ORDER BY d.id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .bind(&role_asset_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    let mut overrides = Vec::new();
    for reference in explicit.iter_mut() {
        if !matches!(reference.prompt_asset.kind.as_str(), "role" | "character") {
            continue;
        }
        let scenes_of = |value: &Value| {
            value
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let candidate = rows.iter().find(|(parent, _, _, scenes)| {
            *parent == reference.asset_id && scene_matches(scene_key, &scenes_of(scenes))
        });
        if let Some((_, derive_id, file_path, _)) = candidate {
            if *file_path != reference.file_path {
                overrides.push(json!({
                    "assetId": reference.asset_id,
                    "deriveId": derive_id,
                    "fromImagePath": reference.file_path,
                    "toImagePath": file_path,
                }));
                reference.file_path = file_path.clone();
            }
        }
    }
    overrides
}

/// Builds one deterministic request plan. Image-edit source frames remain first; persisted
/// storyboard references keep their original relative order after them. Missing scene anchors are
/// appended and described by a server-owned instruction with the resulting marker numbers.
pub(crate) async fn build_storyboard_reference_plan(
    pool: &PgPool,
    project_id: i64,
    script_id: i64,
    storyboard_id: i64,
    explicit: Vec<StoryboardAssetReference>,
    leading_paths: Vec<String>,
) -> Result<StoryboardReferencePlan, AppError> {
    let storyboard = sqlx::query_as::<_, StoryboardBinding>(
        "SELECT scene_key,scene_state_id FROM toonflow.storyboards WHERE id=$1 AND project_id=$2 AND script_id=$3",
    )
    .bind(storyboard_id)
    .bind(project_id)
    .bind(script_id)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::internal("failed to load storyboard scene binding"))?
    .ok_or_else(|| AppError::not_found("storyboard not found"))?;

    let scene_key = storyboard
        .scene_key
        .clone()
        .ok_or_else(|| AppError::bad_request("分镜缺少 sceneKey，请先设置所属场次"))?;
    let mut explicit = explicit;
    let costume_overrides =
        apply_costume_overrides(pool, project_id, script_id, &scene_key, &mut explicit).await;
    let (mut paths, mut marker_by_image) = initialize_reference_paths(&explicit, leading_paths);
    let scene_key = storyboard
        .scene_key
        .ok_or_else(|| AppError::bad_request("分镜缺少 sceneKey，请先设置所属场次"))?;
    let scene_state_id = storyboard.scene_state_id.ok_or_else(|| {
        AppError::bad_request(format!(
            "{scene_key} 的分镜尚未绑定场景状态，请先在“场景一致性”中完成配置"
        ))
    })?;
    let binding = sqlx::query_as::<_, SceneBinding>(
        r#"SELECT master.id AS master_id,master.name AS master_name,
                  master.scene_asset_id AS master_asset_id,master.pinned_image_id AS master_image_id,
                  CASE WHEN image.state='已完成' AND image.file_path<>'' THEN image.file_path END AS master_image_path,
                  master.status AS master_status,master.revision AS master_revision,
                  master.spatial_prompt,master.layout_spec,
                  scene_state.id AS state_id,scene_state.state_key,scene_state.name AS state_name,
                  scene_state.revision AS state_revision
           FROM toonflow.scene_states scene_state
           JOIN toonflow.scene_masters master ON master.id=scene_state.scene_master_id
           LEFT JOIN toonflow.images image ON image.id=master.pinned_image_id
           WHERE scene_state.id=$1 AND master.project_id=$2 AND master.script_id=$3
             AND master.scene_key=$4"#,
    )
    .bind(scene_state_id)
    .bind(project_id)
    .bind(script_id)
    .bind(&scene_key)
    .fetch_optional(pool)
    .await
    .map_err(|_| AppError::internal("failed to load scene consistency binding"))?
    .ok_or_else(|| AppError::bad_request("分镜绑定了其他场次的状态，请重新选择场景状态"))?;
    if binding.master_status != "ready" {
        return Err(AppError::bad_request(format!(
            "{scene_key} 的场景母版尚未就绪，请先在“场景一致性”中选择并确认母版图"
        )));
    }
    let master_image_id = binding
        .master_image_id
        .ok_or_else(|| AppError::bad_request(format!("{scene_key} 的场景母版没有锁定图片版本")))?;
    let master_image_path = binding.master_image_path.clone().ok_or_else(|| {
        AppError::bad_request(format!("{scene_key} 的场景母版图片不可用，请重新确认母版"))
    })?;

    let states = sqlx::query_as::<_, StateConstraint>(
        r#"SELECT id,parent_state_id,state_key,name,change_summary,state_prompt,object_states
           FROM toonflow.scene_states WHERE scene_master_id=$1"#,
    )
    .bind(binding.master_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to load scene state chain"))?;
    let state_chain = state_chain(&states, scene_state_id)?;
    let state_ids = state_chain
        .iter()
        .map(|scene_state| scene_state.id)
        .collect::<Vec<_>>();
    let state_references = if state_ids.is_empty() {
        Vec::new()
    } else {
        sqlx::query_as::<_, StateReference>(
            r#"SELECT reference.sort_order,reference.role,
                      reference.asset_id,reference.image_id,image.file_path,
                      image.state AS image_state,reference.prompt_label
               FROM toonflow.scene_state_references reference
               LEFT JOIN toonflow.images image ON image.id=reference.image_id
               WHERE reference.scene_state_id=ANY($1)
                 AND reference.role<>'master'
               ORDER BY array_position($1::bigint[],reference.scene_state_id),reference.sort_order"#,
        )
        .bind(&state_ids)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to load scene state references"))?
    };
    if let Some(reference) = state_references.iter().find(|reference| {
        reference.image_state.as_deref() != Some("已完成")
            || reference.file_path.as_deref().is_none_or(str::is_empty)
    }) {
        return Err(AppError::bad_request(format!(
            "场景状态参考“{}”的图片不可用，请先修复或移除该参考",
            if reference.prompt_label.trim().is_empty() {
                reference.asset_id.to_string()
            } else {
                reference.prompt_label.clone()
            }
        )));
    }

    let mut managed_references = Vec::new();
    let master_marker = append_reference_if_missing(
        &mut paths,
        &mut marker_by_image,
        master_image_id,
        master_image_path,
    );
    managed_references.push(ManagedReference {
        role: "master",
        marker: master_marker,
        asset_id: binding.master_asset_id,
        image_id: master_image_id,
        label: binding.master_name.clone(),
    });
    for reference in state_references {
        let marker = append_reference_if_missing(
            &mut paths,
            &mut marker_by_image,
            reference.image_id,
            reference.file_path.expect("validated state reference path"),
        );
        managed_references.push(ManagedReference {
            role: if reference.role == "object_detail" {
                "object_detail"
            } else {
                "state"
            },
            marker,
            asset_id: Some(reference.asset_id),
            image_id: reference.image_id,
            label: if reference.prompt_label.trim().is_empty() {
                format!("状态参考 {}", reference.sort_order + 1)
            } else {
                reference.prompt_label
            },
        });
    }
    managed_references.sort_by_key(|reference| {
        (
            match reference.role {
                "master" => 0,
                "state" => 1,
                _ => 2,
            },
            reference.marker,
        )
    });
    managed_references.dedup_by_key(|reference| reference.image_id);

    let instruction =
        consistency_instruction(&scene_key, &binding, &state_chain, &managed_references);
    let reference_manifest = managed_references
        .iter()
        .map(|reference| {
            json!({
                "role": reference.role,
                "marker": reference.marker,
                "assetId": reference.asset_id,
                "imageId": reference.image_id,
                "label": reference.label,
            })
        })
        .collect::<Vec<_>>();
    let reference_count = paths.len();
    Ok(StoryboardReferencePlan {
        paths,
        managed_instruction: Some(instruction),
        scene_state_id: Some(binding.state_id),
        generation_context: json!({
            "mode": "scene_consistency",
            "sceneKey": scene_key,
            "masterId": binding.master_id,
            "masterRevision": binding.master_revision,
            "masterImageId": master_image_id,
            "stateId": binding.state_id,
            "stateKey": binding.state_key,
            "stateRevision": binding.state_revision,
            "managedReferences": reference_manifest,
            "costumeOverrides": costume_overrides,
            "referenceCount": reference_count,
        }),
    })
}

fn initialize_reference_paths(
    explicit: &[StoryboardAssetReference],
    mut leading_paths: Vec<String>,
) -> (Vec<String>, HashMap<i64, usize>) {
    let explicit_offset = leading_paths.len();
    leading_paths.extend(explicit.iter().map(|reference| reference.file_path.clone()));
    let mut marker_by_image = HashMap::new();
    for (index, reference) in explicit.iter().enumerate() {
        marker_by_image
            .entry(reference.image_id)
            .or_insert(explicit_offset + index + 1);
    }
    (leading_paths, marker_by_image)
}

fn append_reference_if_missing(
    paths: &mut Vec<String>,
    marker_by_image: &mut HashMap<i64, usize>,
    image_id: i64,
    path: String,
) -> usize {
    if let Some(marker) = marker_by_image.get(&image_id) {
        *marker
    } else {
        paths.push(path);
        let marker = paths.len();
        marker_by_image.insert(image_id, marker);
        marker
    }
}

fn state_chain<'a>(
    states: &'a [StateConstraint],
    current_state_id: i64,
) -> Result<Vec<&'a StateConstraint>, AppError> {
    let by_id = states
        .iter()
        .map(|scene_state| (scene_state.id, scene_state))
        .collect::<HashMap<_, _>>();
    let mut current_id = Some(current_state_id);
    let mut visited = HashSet::new();
    let mut reverse_chain = Vec::new();
    while let Some(state_id) = current_id {
        if !visited.insert(state_id) {
            return Err(AppError::bad_request("场景状态链存在循环，请修复状态配置"));
        }
        let scene_state = by_id
            .get(&state_id)
            .copied()
            .ok_or_else(|| AppError::bad_request("场景状态链缺少前置状态"))?;
        reverse_chain.push(scene_state);
        current_id = scene_state.parent_state_id;
    }
    reverse_chain.reverse();
    Ok(reverse_chain)
}

fn consistency_instruction(
    scene_key: &str,
    binding: &SceneBinding,
    state_chain: &[&StateConstraint],
    references: &[ManagedReference],
) -> String {
    let master = references
        .iter()
        .find(|reference| reference.role == "master")
        .expect("master reference is always present");
    let state_reference_text = references
        .iter()
        .filter(|reference| reference.role != "master")
        .map(|reference| {
            format!(
                "@图{}（{}，{}）",
                reference.marker,
                if reference.role == "state" {
                    "状态锚"
                } else {
                    "物件状态细节"
                },
                reference.label
            )
        })
        .collect::<Vec<_>>()
        .join("、");
    let changes = state_chain
        .iter()
        .filter_map(|scene_state| {
            let prompt = if scene_state.state_prompt.trim().is_empty() {
                scene_state.change_summary.trim()
            } else {
                scene_state.state_prompt.trim()
            };
            (!prompt.is_empty()).then(|| {
                format!(
                    "{}（{}）：{}",
                    scene_state.state_key, scene_state.name, prompt
                )
            })
        })
        .collect::<Vec<_>>();
    let object_states = state_chain
        .last()
        .filter(|scene_state| scene_state.object_states != json!({}))
        .map(|scene_state| scene_state.object_states.to_string())
        .unwrap_or_default();
    format!(
        "【场景一致性托管约束｜必须执行】\n\
         - 场次：{scene_key}；当前状态：{}（{}）。\n\
         - @图{} 是场景母版“{}”，只用于锁定空间拓扑、门窗与固定家具相对位置、材质和尺度。允许改变景别、机位、透视、人物动作与表情；不要求完整复刻母版景别。\n\
         - 固定空间描述：{}\n\
         - 布局规格：{}\n\
         - 累计状态变化：{}\n\
         - 当前物件状态快照：{}\n\
         - 状态参考：{}\n\
         - 除上述状态变化外，禁止擅自移动、增删、修复或再次破坏场内固定物；不得用上一条分镜图替代母版或继续累积误差。",
        binding.state_key,
        binding.state_name,
        master.marker,
        binding.master_name,
        fallback_text(&binding.spatial_prompt, "以场景母版可见结构为准"),
        if binding.layout_spec == json!({}) {
            "以场景母版可见布局为准".to_string()
        } else {
            binding.layout_spec.to_string()
        },
        if changes.is_empty() {
            "初始状态；所有固定物保持母版中的完好与摆放状态".to_string()
        } else {
            changes.join("；")
        },
        fallback_text(&object_states, "未单独登记，以累计状态变化为准"),
        fallback_text(
            &state_reference_text,
            "无额外状态图，以母版和文字状态约束为准"
        ),
    )
}

fn fallback_text<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(id: i64, parent_state_id: Option<i64>, state_key: &str) -> StateConstraint {
        StateConstraint {
            id,
            parent_state_id,
            state_key: state_key.into(),
            name: state_key.into(),
            change_summary: String::new(),
            state_prompt: String::new(),
            object_states: json!({}),
        }
    }

    #[test]
    fn managed_references_append_without_renumbering_manual_assets() {
        let mut paths = vec!["role.png".into(), "prop.png".into()];
        let mut markers = HashMap::from([(11, 1), (12, 2)]);

        let marker = append_reference_if_missing(&mut paths, &mut markers, 20, "master.png".into());

        assert_eq!(marker, 3);
        assert_eq!(paths, vec!["role.png", "prop.png", "master.png"]);
        assert_eq!(markers.get(&11), Some(&1));
        assert_eq!(markers.get(&12), Some(&2));
    }

    #[test]
    fn scene_matching_normalizes_sc_prefixes_and_treats_empty_as_universal() {
        assert!(scene_matches("sc1", &["场1".into()]));
        assert!(scene_matches("场2", &["sc2".into()]));
        assert!(scene_matches("第3场", &["场3".into(), "场7".into()]));
        assert!(!scene_matches("sc4", &["场1".into()]));
        // 造型 scenes 为空表示适用整个剧本。
        assert!(scene_matches("sc9", &[]));
    }

    #[test]
    fn image_edit_source_stays_before_persisted_storyboard_references() {
        let explicit = vec![
            StoryboardAssetReference {
                asset_id: 1,
                image_id: 11,
                file_path: "role.png".into(),
                prompt_asset: crate::toonflow_asset_context::StoryboardPromptAsset {
                    name: "角色".into(),
                    kind: "role".into(),
                },
            },
            StoryboardAssetReference {
                asset_id: 2,
                image_id: 12,
                file_path: "prop.png".into(),
                prompt_asset: crate::toonflow_asset_context::StoryboardPromptAsset {
                    name: "道具".into(),
                    kind: "tool".into(),
                },
            },
        ];

        let (paths, markers) =
            initialize_reference_paths(&explicit, vec!["storyboard-source.png".into()]);

        assert_eq!(paths, vec!["storyboard-source.png", "role.png", "prop.png"]);
        assert_eq!(markers.get(&11), Some(&2));
        assert_eq!(markers.get(&12), Some(&3));
    }

    #[test]
    fn duplicate_master_image_reuses_existing_marker() {
        let mut paths = vec!["master.png".into(), "role.png".into()];
        let mut markers = HashMap::from([(20, 1), (11, 2)]);

        let marker = append_reference_if_missing(&mut paths, &mut markers, 20, "master.png".into());

        assert_eq!(marker, 1);
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn state_chain_is_cumulative_and_cycle_safe() {
        let states = vec![
            state(1, None, "base"),
            state(2, Some(1), "door_broken"),
            state(3, Some(2), "table_broken"),
        ];
        let keys = state_chain(&states, 3)
            .unwrap()
            .into_iter()
            .map(|scene_state| scene_state.state_key.as_str())
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["base", "door_broken", "table_broken"]);

        let cycle = vec![state(1, Some(2), "a"), state(2, Some(1), "b")];
        assert!(state_chain(&cycle, 1).is_err());
    }
}
