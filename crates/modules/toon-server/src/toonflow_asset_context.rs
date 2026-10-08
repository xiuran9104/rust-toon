use serde::Serialize;
use serde_json::{Value, json};
use sqlx::FromRow;
use std::collections::HashMap;

use rust_toon_framework_web::AppError;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StoryboardPromptAsset {
    pub(crate) name: String,
    pub(crate) kind: String,
}

#[derive(Clone, Debug)]
pub(crate) struct StoryboardAssetReference {
    pub(crate) asset_id: i64,
    pub(crate) image_id: i64,
    pub(crate) file_path: String,
    pub(crate) prompt_asset: StoryboardPromptAsset,
}

#[derive(Clone, Debug, FromRow)]
pub(crate) struct TrackAssetReference {
    pub(crate) asset_id: i64,
    pub(crate) asset_name: String,
    pub(crate) asset_type: String,
    pub(crate) image_id: i64,
    pub(crate) file_path: String,
}

#[derive(Clone, Debug, FromRow)]
struct StoryboardAssetReferenceRow {
    asset_id: i64,
    asset_name: String,
    asset_type: String,
    asset_project_id: i64,
    image_id: Option<i64>,
    file_path: Option<String>,
    image_state: Option<String>,
}

#[derive(Debug, FromRow)]
struct StoryboardPromptAssetRow {
    asset_name: String,
    asset_type: String,
}

/// A production-facing asset record assembled from the canonical asset tables.
#[derive(Debug, FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProductionAsset {
    id: i64,
    name: String,
    #[serde(rename = "type")]
    type_: String,
    description: String,
    prompt: String,
    image_id: Option<i64>,
    image_file_path: Option<String>,
    image_state: Option<String>,
    image_error_reason: Option<String>,
    parent_asset_id: Option<i64>,
    appearance_id: Option<i64>,
    source_project_id: i64,
    source_project_name: String,
}

#[derive(Debug, FromRow, Serialize)]
#[serde(rename_all = "camelCase")]
struct CharacterAppearance {
    id: i64,
    role_asset_id: i64,
    name: String,
    age_stage: String,
    scenes: Value,
    costume_prompt: String,
    description: String,
}

/// Loads the live script text, its explicitly linked assets, and their derived assets for
/// production Agents and Flow UI.
///
/// Assets remain project-owned, while `script_assets` defines which reusable assets participate in
/// the current episode. Reading this projection on demand prevents the Agent workspace from keeping
/// a stale, manually copied asset list.
pub async fn load_script_context(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
) -> Result<(String, Value), sqlx::Error> {
    let script = sqlx::query_scalar::<_, String>(
        "SELECT content FROM toonflow.scripts WHERE id=$1 AND project_id=$2",
    )
    .bind(script_id)
    .bind(project_id)
    .fetch_optional(pool)
    .await?
    .unwrap_or_default();
    let asset_rows = sqlx::query_as::<_, ProductionAsset>(
        r#"WITH linked_assets AS (
             SELECT asset_id FROM toonflow.script_assets WHERE script_id=$1
           )
           SELECT a.id, a.name, a.type AS type_, a.description, a.prompt,
                  a.image_id, selected_image.file_path AS image_file_path,
                  latest_image.state AS image_state,
                  latest_image.error_reason AS image_error_reason,
                  a.parent_asset_id, a.appearance_id,
                  a.project_id AS source_project_id, p.name AS source_project_name
           FROM toonflow.assets a
           JOIN toonflow.projects p ON p.id=a.project_id
           LEFT JOIN toonflow.images selected_image ON selected_image.id=a.image_id
           LEFT JOIN LATERAL (
             SELECT i.state,i.error_reason
             FROM toonflow.images i
             WHERE i.assets_id=a.id
             ORDER BY i.id DESC
             LIMIT 1
           ) latest_image ON TRUE
           WHERE a.project_id=$2 AND (
             a.id IN (SELECT asset_id FROM linked_assets)
             OR a.parent_asset_id IN (SELECT asset_id FROM linked_assets)
           )
           ORDER BY a.parent_asset_id NULLS FIRST, a.type, a.name, a.id"#,
    )
    .bind(script_id)
    .bind(project_id)
    .fetch_all(pool)
    .await?;
    let appearance_rows = sqlx::query_as::<_, CharacterAppearance>(
        r#"SELECT id,role_asset_id,name,age_stage,scenes,costume_prompt,description
           FROM toonflow.character_appearances
           WHERE project_id=$1 AND script_id=$2
           ORDER BY role_asset_id,id"#,
    )
    .bind(project_id)
    .bind(script_id)
    .fetch_all(pool)
    .await?;
    let mut derived_by_parent: HashMap<i64, Vec<&ProductionAsset>> = HashMap::new();
    for asset in &asset_rows {
        if let Some(parent_id) = asset.parent_asset_id {
            derived_by_parent.entry(parent_id).or_default().push(asset);
        }
    }
    let mut appearances_by_role: HashMap<i64, Vec<&CharacterAppearance>> = HashMap::new();
    for appearance in &appearance_rows {
        appearances_by_role
            .entry(appearance.role_asset_id)
            .or_default()
            .push(appearance);
    }
    let assets = asset_rows
        .iter()
        .filter(|asset| asset.parent_asset_id.is_none())
        .map(|asset| {
            let mut value = json!(asset);
            value["derive"] = json!(
                derived_by_parent
                    .get(&asset.id)
                    .cloned()
                    .unwrap_or_default()
            );
            value["appearances"] = json!(
                appearances_by_role
                    .get(&asset.id)
                    .cloned()
                    .unwrap_or_default()
            );
            value
        })
        .collect::<Vec<_>>();
    Ok((script, json!(assets)))
}

/// Returns usable image URLs for every asset explicitly associated with storyboards on a track.
pub async fn load_track_asset_references(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    track_id: i64,
) -> Result<Vec<String>, sqlx::Error> {
    Ok(load_track_asset_reference_manifest(pool, project_id, script_id, track_id)
        .await?
        .into_iter()
        .map(|reference| reference.file_path)
        .collect())
}

/// P0.4 参考图上限策略：当前镜头主体 > 关键道具 > 场景参考。同类内保持分镜
/// 首现顺序，被舍弃的引用连同原因返回，写入生成快照供任务详情展示。
pub async fn load_track_asset_reference_selection(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    track_id: i64,
    cap: usize,
) -> Result<(Vec<TrackAssetReference>, Vec<Value>), sqlx::Error> {
    let manifest = load_track_asset_reference_manifest(pool, project_id, script_id, track_id).await?;
    Ok(select_references_within_cap(manifest, cap))
}

fn reference_priority(asset_type: &str) -> u8 {
    match asset_type {
        "role" | "character" => 0,
        "scene" => 2,
        _ => 1,
    }
}

pub(crate) fn select_references_within_cap(
    references: Vec<TrackAssetReference>,
    cap: usize,
) -> (Vec<TrackAssetReference>, Vec<Value>) {
    if references.len() <= cap {
        return (references, Vec::new());
    }
    let mut ranked: Vec<(u8, usize)> = references
        .iter()
        .enumerate()
        .map(|(position, reference)| (reference_priority(&reference.asset_type), position))
        .collect();
    ranked.sort_unstable();
    let kept: std::collections::HashSet<usize> = ranked
        .into_iter()
        .take(cap)
        .map(|(_, position)| position)
        .collect();
    let mut selected = Vec::with_capacity(cap);
    let mut dropped = Vec::new();
    for (position, reference) in references.into_iter().enumerate() {
        if kept.contains(&position) {
            selected.push(reference);
        } else {
            dropped.push(json!({
                "assetId": reference.asset_id,
                "imageId": reference.image_id,
                "name": reference.asset_name,
                "assetType": reference.asset_type,
                "filePath": reference.file_path,
                "reason": "参考图超过上限，按优先级保留镜头主体、关键道具和场景参考后舍弃",
            }));
        }
    }
    (selected, dropped)
}

/// Loads the canonical video-reference manifest in storyboard first-use order.
pub async fn load_track_asset_reference_manifest(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    track_id: i64,
) -> Result<Vec<TrackAssetReference>, sqlx::Error> {
    sqlx::query_as(
        r#"SELECT asset_id,asset_name,asset_type,image_id,file_path
           FROM (
             SELECT DISTINCT ON (i.file_path)
                    a.id AS asset_id,a.name AS asset_name,a.type AS asset_type,
                    i.id AS image_id,i.file_path,
                    s.id AS storyboard_id,ast.sort_order
           FROM toonflow.storyboards s
           JOIN toonflow.assets_storyboards ast ON ast.storyboard_id=s.id
           JOIN toonflow.assets a ON a.id=ast.asset_id
           JOIN toonflow.images i ON i.id=a.image_id
           WHERE s.track_id=$1 AND s.project_id=$2 AND s.script_id=$3
             AND a.project_id=$2
             AND i.file_path IS NOT NULL AND i.file_path <> ''
             ORDER BY i.file_path,s.id,ast.sort_order,a.id
           ) ordered_references
           ORDER BY storyboard_id,sort_order,asset_id"#,
    )
    .bind(track_id)
    .bind(project_id)
    .bind(script_id)
    .fetch_all(pool)
    .await
}

/// Loads the ordered asset metadata used by the persisted `@图N` prompt contract.
pub async fn load_storyboard_prompt_assets(
    pool: &sqlx::PgPool,
    project_id: i64,
    asset_ids: &[i64],
) -> Result<Vec<StoryboardPromptAsset>, AppError> {
    if asset_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, StoryboardPromptAssetRow>(
        r#"SELECT asset.name AS asset_name,asset.type AS asset_type
           FROM unnest($2::bigint[]) WITH ORDINALITY requested(id,position)
           JOIN toonflow.assets asset ON asset.id=requested.id AND asset.project_id=$1
           ORDER BY requested.position"#,
    )
    .bind(project_id)
    .bind(asset_ids)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to load storyboard prompt assets"))?;
    if rows.len() != asset_ids.len() {
        return Err(AppError::bad_request(
            "分镜包含不存在或不属于当前项目的关联资产，请重新保存资产绑定",
        ));
    }
    Ok(rows
        .into_iter()
        .map(|row| StoryboardPromptAsset {
            name: row.asset_name,
            kind: row.asset_type,
        })
        .collect())
}

/// Returns the current image of each asset associated with one storyboard in the exact order used
/// by the persisted `@图N` prompt contract.
pub async fn load_storyboard_asset_references(
    pool: &sqlx::PgPool,
    project_id: i64,
    script_id: i64,
    storyboard_id: i64,
) -> Result<Vec<StoryboardAssetReference>, AppError> {
    let rows = sqlx::query_as::<_, StoryboardAssetReferenceRow>(
        r#"SELECT a.id AS asset_id,a.name AS asset_name,a.type AS asset_type,a.project_id AS asset_project_id,
                  i.id AS image_id,i.file_path,i.state AS image_state
           FROM toonflow.assets_storyboards ast
           JOIN toonflow.storyboards s ON s.id=ast.storyboard_id
           JOIN toonflow.assets a ON a.id=ast.asset_id
           LEFT JOIN toonflow.images i ON i.id=a.image_id
           WHERE ast.storyboard_id=$1 AND s.project_id=$2 AND s.script_id=$3
           ORDER BY ast.sort_order, ast.asset_id"#,
    )
    .bind(storyboard_id)
    .bind(project_id)
    .bind(script_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to load storyboard references"))?;
    if rows.iter().any(|row| row.asset_project_id != project_id) {
        return Err(AppError::bad_request(
            "分镜包含其他项目的关联资产，请重新保存当前项目的资产绑定",
        ));
    }
    let unavailable = rows
        .iter()
        .filter(|row| {
            row.image_id.is_none()
                || row.image_state.as_deref() != Some("已完成")
                || row.file_path.as_deref().is_none_or(str::is_empty)
        })
        .map(|row| row.asset_name.as_str())
        .collect::<Vec<_>>();
    if !unavailable.is_empty() {
        return Err(AppError::bad_request(format!(
            "分镜关联资产尚无可用图片：{}。请先完成这些资产图片，不能跳过后重新编号 @图N。",
            unavailable.join("、")
        )));
    }
    Ok(rows
        .into_iter()
        .map(|row| StoryboardAssetReference {
            asset_id: row.asset_id,
            image_id: row.image_id.expect("validated storyboard image id"),
            file_path: row.file_path.expect("validated storyboard image path"),
            prompt_asset: StoryboardPromptAsset {
                name: row.asset_name,
                kind: row.asset_type,
            },
        })
        .collect())
}
