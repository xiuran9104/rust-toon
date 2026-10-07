mod ai_client;
mod episodes;
mod projects;
mod scenes;
mod shared;
mod toonflow;
mod toonflow_agent_episode_scope;
mod toonflow_agent_events;
mod toonflow_agent_history;
mod toonflow_agent_plan;
mod toonflow_agent_read_tools;
mod toonflow_script_quality;
mod toonflow_agent_runtime;
mod toonflow_agent_tool_record;
mod toonflow_agent_tool_utils;
mod toonflow_agent_tools;
mod toonflow_agents;
mod toonflow_asset_ai;
mod toonflow_asset_context;
mod toonflow_asset_library;
mod toonflow_asset_prompt;
mod toonflow_prompt_trace;
#[cfg(test)]
mod toonflow_alignment_tests;
mod toonflow_image_contract;
mod toonflow_face_identity;
mod toonflow_audio;
mod toonflow_character_identity;
mod toonflow_episode_renders;
mod toonflow_image_edit_prompt;
mod toonflow_image_workflow;
mod toonflow_manuals;
mod toonflow_materials;
mod toonflow_novel_events;
mod toonflow_pagination;
mod toonflow_project;
mod toonflow_project_crud;
mod toonflow_project_helpers;
mod toonflow_prompt_store;
mod toonflow_resources;
mod toonflow_scene_analysis_parse;
mod toonflow_scene_consistency;
mod toonflow_scene_consistency_auto;
mod toonflow_scene_state_plan;
mod toonflow_scene_transitions;
mod toonflow_script_ai;
mod toonflow_status;
mod toonflow_storage;
mod toonflow_storyboard_asset_validation;
mod toonflow_storyboard_panel_validation;
mod toonflow_storyboard_prompt_validation;
mod toonflow_storyboard_references;
mod toonflow_storyboard_table_validation;
mod toonflow_video;
mod toonflow_video_compilers;
mod toonflow_video_continuity;
mod toonflow_video_export;
mod toonflow_video_quality;
mod toonflow_workflow;
mod toonflow_workflow_control;
mod toonflow_workflow_definition;
mod toonflow_workflow_utils;
mod toonflow_ws;

#[cfg(test)]
mod production_e2e_tests;
#[cfg(test)]
mod provider_e2e_tests;

use axum::{
    Json, Router,
    middleware::from_fn_with_state,
    routing::{get, post, put},
};
use futures_util::{StreamExt, stream};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_database::PgPool;
use rust_toon_framework_security::{TokenService, authenticate};
use rust_toon_toon_api::ToonCapability;

pub use toonflow_video_export::{
    VIDEO_EXPORT_JOB_KIND, cleanup_stale_export_workdirs, execute_distributed_export,
};
pub use toonflow_video_quality::{VIDEO_QUALITY_JOB_KIND, execute_distributed_quality};
pub use toonflow_video::resume_interrupted_video_generations;
pub use toonflow_workflow::resume_interrupted_workflow_runs;

const STORAGE_CLEANUP_CONCURRENCY: usize = 8;
const STORAGE_CLEANUP_REFERENCE_RECHECK_SECONDS: u64 = 30;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StorageObjectReference {
    None,
    Permanent,
    ActiveJob,
}

#[derive(Clone)]
pub struct ToonState {
    pool: PgPool,
    tokens: TokenService,
}

/// Verify the configured object-store credentials and bucket using the same
/// signed S3 requests as Toonflow's production media pipeline.
pub async fn check_object_storage_readiness() -> Result<(), String> {
    toonflow_storage::check_bucket_readiness().await
}

/// Initialize the configured object-store bucket once during service startup.
/// Readiness probes remain side-effect free and only perform a signed HEAD.
pub async fn initialize_object_storage() -> Result<(), String> {
    toonflow_storage::initialize_bucket().await
}

/// Remove provider-download files left by a previous Gateway process. This is
/// safe only during startup, before this process can own an active download.
pub async fn cleanup_provider_video_temp_on_startup() -> Result<u64, String> {
    toonflow_storage::cleanup_provider_video_temp_on_startup().await
}

/// Mark only Gateway-owned in-process work as interrupted after a restart.
/// Durable media tasks are deliberately excluded and remain owned by the
/// worker lease/reaper protocol. Workflow runs and video generations that can
/// be resumed (video.generate nodes, videos with a persisted provider task)
/// are also excluded; `resume_interrupted_video_generations` and
/// `resume_interrupted_workflow_runs` re-drive them after this repair.
pub async fn repair_gateway_interrupted_state(pool: &PgPool) -> Result<u64, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let mut repaired = 0_u64;
    // Node executions that cannot be checkpointed (LLM tool loops, storyboard
    // image jobs) still fail on restart. video.generate nodes are excluded:
    // their videos resume via persisted provider tasks or durable quality
    // jobs, and a resumed waiter re-attaches the node run.
    let interrupted_nodes: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT id,workflow_run_id FROM toonflow.workflow_node_runs
         WHERE state='running' AND node_type<>'video.generate'",
    )
    .fetch_all(&mut *tx)
    .await?;
    let node_ids = interrupted_nodes.iter().map(|row| row.0).collect::<Vec<_>>();
    let run_ids = interrupted_nodes
        .iter()
        .map(|row| row.1)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if !node_ids.is_empty() {
        repaired += sqlx::query("UPDATE toonflow.workflow_node_runs SET state='failed',error_reason='服务重启导致任务中断',finish_time=(extract(epoch from clock_timestamp())*1000)::bigint WHERE id=ANY($1) AND state='running'")
            .bind(&node_ids)
            .execute(&mut *tx).await?.rows_affected();
        repaired += sqlx::query("UPDATE toonflow.workflow_runs SET state='failed',error_reason='服务重启导致任务中断',finish_time=(extract(epoch from clock_timestamp())*1000)::bigint WHERE id=ANY($1) AND state='running'")
            .bind(&run_ids)
            .execute(&mut *tx).await?.rows_affected();
    }
    repaired += sqlx::query("UPDATE toonflow.novels SET event_state=-1,error_reason='服务重启导致失败' WHERE event_state=0")
        .execute(&mut *tx).await?.rows_affected();
    repaired += sqlx::query("UPDATE toonflow.assets SET prompt_state='生成失败',prompt_error_reason='服务重启导致失败' WHERE prompt_state='生成中'")
        .execute(&mut *tx).await?.rows_affected();
    repaired += sqlx::query("UPDATE toonflow.images SET state='生成失败',error_reason='服务重启导致失败' WHERE state='生成中'")
        .execute(&mut *tx).await?.rows_affected();
    repaired += sqlx::query("UPDATE toonflow.storyboards SET state='生成失败',reason='服务重启导致失败' WHERE state='生成中'")
        .execute(&mut *tx).await?.rows_affected();
    repaired += sqlx::query("UPDATE toonflow.video_tracks SET state='生成失败',reason='服务重启导致失败' WHERE state='生成中'")
        .execute(&mut *tx).await?.rows_affected();
    repaired += sqlx::query("UPDATE toonflow.videos v SET state='生成失败',error_reason='服务重启导致失败' WHERE state='生成中' AND v.generation_context->'provider'->>'taskId' IS NULL AND NOT EXISTS(SELECT 1 FROM toonflow.distributed_jobs j WHERE j.kind='toon.video_quality' AND j.task_id::text=v.generation_context->'quality'->>'taskId' AND j.state IN ('queued','running','retry'))")
        .execute(&mut *tx).await?.rows_affected();
    repaired += sqlx::query(
        "UPDATE toonflow.tasks tasks
         SET state='failed',reason='服务重启导致失败'
         WHERE tasks.state='running'
           -- An active durable job survives a Gateway restart and remains the
           -- worker/reaper's responsibility. Terminal jobs are intentionally
           -- not exempt: their task row should already have been finalized in
           -- the same worker transaction, so a still-running task is stale.
           AND NOT EXISTS(
             SELECT 1 FROM toonflow.distributed_jobs jobs
             WHERE jobs.task_id=tasks.id
               AND jobs.state IN ('queued','retry','running')
           )",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(repaired)
}

#[cfg(test)]
mod gateway_repair_database_tests {
    use std::time::Duration;

    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};

    use super::repair_gateway_interrupted_state;

    async fn insert_running_task(pool: &sqlx::PgPool, description: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO toonflow.tasks(
               task_class,description,state,start_time,reason
             )
             VALUES('gatewayRepairTest',$1,'running',
                    (extract(epoch from clock_timestamp())*1000)::bigint,
                    'original reason')
             RETURNING id",
        )
        .bind(description)
        .fetch_one(pool)
        .await
        .expect("insert running task fixture")
    }

    async fn insert_distributed_job(pool: &sqlx::PgPool, task_id: i64, state: &str) {
        if state == "running" {
            sqlx::query(
                "INSERT INTO toonflow.distributed_jobs(
                   task_id,kind,trace_id,payload,state,
                   lease_owner,lease_token,lease_until,heartbeat_at
                 )
                 VALUES($1,'video.export','gateway-repair-test','{}'::jsonb,$2,
                        'gateway-repair-worker',gen_random_uuid(),
                        now()+interval '5 minutes',now())",
            )
            .bind(task_id)
            .bind(state)
            .execute(pool)
            .await
            .expect("insert running distributed job fixture");
        } else {
            sqlx::query(
                "INSERT INTO toonflow.distributed_jobs(
                   task_id,kind,trace_id,payload,state
                 )
                 VALUES($1,'video.export','gateway-repair-test','{}'::jsonb,$2)",
            )
            .bind(task_id)
            .bind(state)
            .execute(pool)
            .await
            .expect("insert queued or retry distributed job fixture");
        }
    }

    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn only_repairs_running_tasks_without_an_active_distributed_job() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10))
            .expect("valid test database config");
        let pool = connect(&config).await.expect("connect test database");
        migrate(&pool).await.expect("apply migrations");

        let local_task_id = insert_running_task(&pool, "local Gateway task").await;
        let mut durable_tasks = Vec::new();
        for job_state in ["queued", "retry", "running"] {
            let task_id = insert_running_task(&pool, &format!("durable {job_state} task")).await;
            insert_distributed_job(&pool, task_id, job_state).await;
            durable_tasks.push((task_id, job_state));
        }

        let repaired = repair_gateway_interrupted_state(&pool)
            .await
            .expect("repair interrupted Gateway state");
        assert!(repaired >= 1, "the local running task must be repaired");

        let local: (String, Option<String>) =
            sqlx::query_as("SELECT state,reason FROM toonflow.tasks WHERE id=$1")
                .bind(local_task_id)
                .fetch_one(&pool)
                .await
                .expect("load repaired local task");
        assert_eq!(local.0, "failed");
        assert_eq!(local.1.as_deref(), Some("服务重启导致失败"));

        for (task_id, expected_job_state) in &durable_tasks {
            let durable: (String, Option<String>, String) = sqlx::query_as(
                "SELECT tasks.state,tasks.reason,jobs.state
                 FROM toonflow.tasks tasks
                 JOIN toonflow.distributed_jobs jobs ON jobs.task_id=tasks.id
                 WHERE tasks.id=$1",
            )
            .bind(task_id)
            .fetch_one(&pool)
            .await
            .expect("load protected durable task");
            assert_eq!(durable.0, "running", "{expected_job_state} task changed");
            assert_eq!(
                durable.1.as_deref(),
                Some("original reason"),
                "{expected_job_state} task reason changed"
            );
            assert_eq!(&durable.2, expected_job_state);
        }

        let task_ids = std::iter::once(local_task_id)
            .chain(durable_tasks.into_iter().map(|(task_id, _)| task_id))
            .collect::<Vec<_>>();
        sqlx::query("DELETE FROM toonflow.tasks WHERE id=ANY($1)")
            .bind(&task_ids)
            .execute(&pool)
            .await
            .expect("remove Gateway repair fixtures");
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StorageCleanupResult {
    pub completed: u64,
    pub deferred: u64,
    pub failed: u64,
}

/// Claim and process object cleanup records using database leases. Multiple
/// worker replicas may call this concurrently; `SKIP LOCKED` and the fencing
/// token ensure only the current owner can commit a result.
pub async fn process_storage_cleanup_batch(
    pool: &PgPool,
    worker_id: &str,
    limit: i64,
    lease: std::time::Duration,
    operation_timeout: std::time::Duration,
) -> Result<StorageCleanupResult, sqlx::Error> {
    let rows: Vec<(i64, String, uuid::Uuid)> = sqlx::query_as(
        "WITH candidates AS (
           SELECT id FROM toonflow.storage_cleanup_tasks
           WHERE (
             state='pending' AND next_attempt_at <= now() AND attempts < max_attempts
           ) OR (
             state='running' AND lease_until <= now() AND attempts < max_attempts
           )
           ORDER BY update_time,id
           FOR UPDATE SKIP LOCKED
           LIMIT $1
         )
         UPDATE toonflow.storage_cleanup_tasks cleanup
         SET state='running',lease_owner=$2,lease_token=gen_random_uuid(),
             lease_until=now()+make_interval(secs => $3::double precision),
             update_time=(extract(epoch from clock_timestamp())*1000)::bigint
         FROM candidates
         WHERE cleanup.id=candidates.id
         RETURNING cleanup.id,cleanup.object_path,cleanup.lease_token",
    )
    .bind(limit.clamp(1, STORAGE_CLEANUP_CONCURRENCY as i64))
    .bind(worker_id)
    .bind(lease.as_secs_f64())
    .fetch_all(pool)
    .await?;
    let outcomes = stream::iter(rows.into_iter().map(|(id, object_path, lease_token)| {
        let pool = pool.clone();
        async move {
            process_storage_cleanup_row(
                &pool,
                id,
                object_path,
                lease_token,
                lease,
                operation_timeout,
            )
            .await
        }
    }))
    .buffer_unordered(STORAGE_CLEANUP_CONCURRENCY)
    .collect::<Vec<_>>()
    .await;
    let mut result = StorageCleanupResult::default();
    for outcome in outcomes {
        let (completed, deferred, failed) = outcome?;
        result.completed += completed;
        result.deferred += deferred;
        result.failed += failed;
    }
    Ok(result)
}

async fn process_storage_cleanup_row(
    pool: &PgPool,
    id: i64,
    object_path: String,
    lease_token: uuid::Uuid,
    lease: std::time::Duration,
    operation_timeout: std::time::Duration,
) -> Result<(u64, u64, u64), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let fence_seconds = lease
        .as_secs_f64()
        .max(operation_timeout.as_secs_f64() + 5.0);
    let still_owned: bool = sqlx::query_scalar(
        "WITH renewed AS (
           UPDATE toonflow.storage_cleanup_tasks
           SET lease_until=now()+make_interval(secs => $3::double precision),
               update_time=(extract(epoch from clock_timestamp())*1000)::bigint
           WHERE id=$1 AND state='running' AND lease_token=$2
             AND lease_until > now()
           RETURNING 1
         )
         SELECT EXISTS(SELECT 1 FROM renewed)",
    )
    .bind(id)
    .bind(lease_token)
    .bind(fence_seconds)
    .fetch_one(&mut *tx)
    .await?;
    if !still_owned {
        tx.rollback().await?;
        return Ok((0, 0, 0));
    }
    match storage_object_reference(&mut tx, &object_path).await? {
        StorageObjectReference::Permanent => {
            let updated = sqlx::query(
                "UPDATE toonflow.storage_cleanup_tasks
                 SET state='completed',lease_owner=NULL,lease_token=NULL,lease_until=NULL,
                     error_reason='对象仍被业务记录引用（永久引用），已安全跳过删除',next_attempt_at=now(),
                     update_time=(extract(epoch from clock_timestamp())*1000)::bigint
                 WHERE id=$1 AND state='running' AND lease_token=$2",
            )
            .bind(id)
            .bind(lease_token)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok((updated.rows_affected(), 0, 0));
        }
        StorageObjectReference::ActiveJob => {
            let updated = sqlx::query(
                "UPDATE toonflow.storage_cleanup_tasks
                 SET state='pending',lease_owner=NULL,lease_token=NULL,lease_until=NULL,
                     error_reason='对象被非终态分布式任务临时引用，已延后清理',
                     next_attempt_at=now()+make_interval(secs => $3::double precision),
                     update_time=(extract(epoch from clock_timestamp())*1000)::bigint
                 WHERE id=$1 AND state='running' AND lease_token=$2",
            )
            .bind(id)
            .bind(lease_token)
            .bind(STORAGE_CLEANUP_REFERENCE_RECHECK_SECONDS as f64)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok((0, updated.rows_affected(), 0));
        }
        StorageObjectReference::None => {}
    }
    tx.commit().await?;

    let deletion = match tokio::time::timeout(
        operation_timeout,
        toonflow_storage::delete_asset_file(&object_path),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => Err(format!(
            "删除对象存储对象超时（{} 秒）",
            operation_timeout.as_secs()
        )),
    };
    match deletion {
        Ok(()) => {
            let updated = sqlx::query(
                "UPDATE toonflow.storage_cleanup_tasks
                 SET state='completed',lease_owner=NULL,lease_token=NULL,lease_until=NULL,
                     error_reason='',next_attempt_at=now(),
                     update_time=(extract(epoch from clock_timestamp())*1000)::bigint
                 WHERE id=$1 AND state='running' AND lease_token=$2
                   AND lease_until > now()",
            )
            .bind(id)
            .bind(lease_token)
            .execute(pool)
            .await?;
            Ok((updated.rows_affected(), 0, 0))
        }
        Err(error) => {
            let error: String = error.chars().take(4_000).collect();
            let state: Option<String> = sqlx::query_scalar(
                "UPDATE toonflow.storage_cleanup_tasks
                 SET state=CASE
                       WHEN attempts + 1 >= max_attempts THEN 'failed' ELSE 'pending'
                     END,
                     attempts=LEAST(attempts + 1,max_attempts),error_reason=$3,
                     next_attempt_at=CASE
                       WHEN attempts + 1 >= max_attempts THEN now()
                       ELSE now() + make_interval(
                         secs => LEAST(3600::double precision,
                           power(2::double precision,LEAST(attempts,10)))
                       )
                     END,
                     lease_owner=NULL,lease_token=NULL,lease_until=NULL,
                     update_time=(extract(epoch from clock_timestamp())*1000)::bigint
                 WHERE id=$1 AND state='running' AND lease_token=$2
                   AND lease_until > now()
                 RETURNING state",
            )
            .bind(id)
            .bind(lease_token)
            .bind(error)
            .fetch_optional(pool)
            .await?;
            Ok(match state.as_deref() {
                Some("failed") => (0, 0, 1),
                Some("pending") => (0, 1, 0),
                _ => (0, 0, 0),
            })
        }
    }
}

async fn storage_object_reference(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    object_path: &str,
) -> Result<StorageObjectReference, sqlx::Error> {
    let object_key = toonflow_storage::asset_object_key(object_path).unwrap_or(object_path);
    let direct_asset_path = format!("/toonflow/assets/files/{object_key}");
    let gateway_asset_path = format!("/api/toonflow/assets/files/{object_key}");
    let (permanent, active_job): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS(
           SELECT 1 FROM toonflow.images
           WHERE file_path IN ($1,$2,$3,$4)
           UNION ALL
           SELECT 1 FROM toonflow.storyboards
           WHERE file_path IN ($1,$2,$3,$4)
           UNION ALL
           SELECT 1 FROM toonflow.videos
           WHERE file_path IN ($1,$2,$3,$4)
           UNION ALL
           SELECT 1 FROM toonflow.video_continuity_frames
           WHERE file_path IN ($1,$2,$3,$4)
           UNION ALL
           SELECT 1 FROM toonflow.episode_renders
           WHERE file_path IN ($1,$2,$3,$4) OR cover_path IN ($1,$2,$3,$4)
              OR object_path IN ($1,$2,$3,$4)
         ), EXISTS(
           SELECT 1 FROM toonflow.distributed_jobs
           WHERE state IN ('queued','retry','running')
             AND result->>'stagingObjectPath' IN ($1,$2,$3,$4)
           UNION ALL
           SELECT 1
           FROM toonflow.distributed_jobs jobs
           CROSS JOIN LATERAL jsonb_array_elements(
             CASE
               WHEN jsonb_typeof(jobs.payload->'sources')='array'
                 THEN jobs.payload->'sources'
               ELSE '[]'::jsonb
             END
           ) AS source(value)
           WHERE jobs.state IN ('queued','retry','running')
             AND source.value->>'filePath' IN ($1,$2,$3,$4)
         )",
    )
    .bind(object_path)
    .bind(object_key)
    .bind(&direct_asset_path)
    .bind(&gateway_asset_path)
    .fetch_one(&mut **tx)
    .await?;
    Ok(if permanent {
        StorageObjectReference::Permanent
    } else if active_job {
        StorageObjectReference::ActiveJob
    } else {
        StorageObjectReference::None
    })
}

#[cfg(test)]
mod storyboard_database_tests {
    use std::time::Duration;

    use axum::{Json, extract::State};
    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
    use rust_toon_framework_security::{
        CurrentUser, DataScope, Permission, PermissionSet, SecurityConfig, TokenService,
    };

    use super::{ToonState, toonflow, toonflow_image_workflow};

    fn user() -> CurrentUser {
        CurrentUser {
            user_id: "storyboard-test".into(),
            username: "storyboard-test".into(),
            tenant_id: None,
            role_codes: vec!["admin".into()],
            permissions: PermissionSet::new([
                Permission::new("toon:scene:create").unwrap(),
                Permission::new("toon:scene:delete").unwrap(),
                Permission::new("toon:scene:read").unwrap(),
                Permission::new("toon:scene:update").unwrap(),
            ]),
            data_scope: DataScope::All,
        }
    }

    fn storyboard(id: i64, track: &str, duration: i64) -> toonflow::SaveStoryboardRequest {
        toonflow::SaveStoryboardRequest {
            id: Some(id),
            prompt: format!("storyboard {id}"),
            duration: Some(duration),
            state: "未生成".into(),
            video_desc: Some(format!("shot {id}")),
            scene_key: None,
            scene_state_id: None,
            scene_state_key: None,
            scene_state_parent_key: None,
            scene_state_description: None,
            should_generate_image: 1,
            file_path: None,
            script_id: None,
            project_id: None,
            track: Some(track.into()),
            associate_assets_ids: vec![],
        }
    }

    #[tokio::test]
    #[ignore = "run with script/test-storyboard-sync.sh"]
    async fn groups_edits_and_deletes_storyboard_tracks_transactionally() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();
        let project_id = 9_100_001_i64;
        let script_id = 9_100_002_i64;
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO toonflow.projects(id,name,create_time,update_time) VALUES($1,'storyboard test',0,0)")
            .bind(project_id).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO toonflow.scripts(id,name,project_id,create_time) VALUES($1,'episode',$2,0)")
            .bind(script_id).bind(project_id).execute(&pool).await.unwrap();
        let tokens = TokenService::new(
            SecurityConfig::new(
                "storyboard-test-secret-at-least-32-bytes",
                "test",
                "test",
                Duration::from_secs(60),
            )
            .unwrap(),
        );
        let state = ToonState::new(pool.clone(), tokens);

        let _ = toonflow::batch_add_storyboards(
            user(),
            State(state.clone()),
            Json(toonflow::BatchStoryboardRequest {
                project_id,
                script_id,
                data: vec![
                    storyboard(9_100_010, "main", 3),
                    storyboard(9_100_011, "main", 5),
                ],
            }),
        )
        .await
        .unwrap();
        let grouped: Vec<(Option<i64>,)> = sqlx::query_as(
            "SELECT track_id FROM toonflow.storyboards WHERE project_id=$1 ORDER BY id",
        )
        .bind(project_id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].0, grouped[1].0);
        let main_track_id = grouped[0].0.unwrap();
        let main_duration: i32 =
            sqlx::query_scalar("SELECT duration FROM toonflow.video_tracks WHERE id=$1")
                .bind(main_track_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(main_duration, 8);

        let _ = toonflow::edit_storyboard_info(
            user(),
            State(state.clone()),
            Json(toonflow::EditStoryboardInfoRequest {
                id: 9_100_010,
                prompt: "edited".into(),
                video_desc: "edited shot".into(),
                scene_key: None,
                scene_state_id: None,
                scene_state_key: None,
                scene_state_parent_key: None,
                scene_state_description: None,
                duration: Some(7),
                track: Some("secondary".into()),
                should_generate_image: Some(1),
                associate_assets_ids: Some(vec![]),
            }),
        )
        .await
        .unwrap();
        let durations: Vec<(String, i32)> = sqlx::query_as(
            "SELECT s.track,t.duration FROM toonflow.storyboards s JOIN toonflow.video_tracks t ON t.id=s.track_id WHERE s.project_id=$1 ORDER BY s.track",
        )
        .bind(project_id).fetch_all(&pool).await.unwrap();
        assert_eq!(durations, vec![("main".into(), 5), ("secondary".into(), 7)]);

        let flow_id = 9_100_020_i64;
        sqlx::query("INSERT INTO toonflow.image_flows(id,flow_data) VALUES($1,'{}')")
            .bind(flow_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("UPDATE toonflow.storyboards SET flow_id=$2 WHERE id=$1")
            .bind(9_100_010_i64)
            .bind(flow_id)
            .execute(&pool)
            .await
            .unwrap();
        let _ = toonflow_image_workflow::delete_storyboards(
            user(),
            State(state),
            Json(toonflow_image_workflow::DeleteStoryboards {
                ids: vec![9_100_010, 9_100_011],
                project_id,
            }),
        )
        .await
        .unwrap();
        let storyboard_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM toonflow.storyboards WHERE project_id=$1")
                .bind(project_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let track_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM toonflow.video_tracks WHERE project_id=$1")
                .bind(project_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        let flow_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM toonflow.image_flows WHERE id=$1")
                .bind(flow_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!((storyboard_count, track_count, flow_count), (0, 0, 0));
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod agent_memory_database_tests {
    use std::time::Duration;

    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
    use rust_toon_framework_security::{SecurityConfig, TokenService};
    use serde_json::json;

    use super::{
        StorageObjectReference, ToonState, process_storage_cleanup_batch, storage_object_reference,
        toonflow_agent_runtime, toonflow_agent_tools, toonflow_agents,
    };

    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn memory_cascades_retrieval_expansion_and_script_upsert_match_toonflow() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();
        let project_id = 9_200_001_i64;
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO toonflow.projects(id,name,create_time,update_time) VALUES($1,'agent memory test',0,0)")
            .bind(project_id).execute(&pool).await.unwrap();
        let isolation = "test:memory:cascade";
        sqlx::query("DELETE FROM toonflow.agent_memories WHERE isolation_key=$1")
            .bind(isolation)
            .execute(&pool)
            .await
            .unwrap();
        for (id, content, summarized) in
            [(9_200_010_i64, "first", true), (9_200_011, "second", true)]
        {
            sqlx::query("INSERT INTO toonflow.agent_memories(id,agent_type,isolation_key,role,content,memory_type,summarized,create_time) VALUES($1,'scriptAgent',$2,'user',$3,'message',$4,$1)")
                .bind(id).bind(isolation).bind(content).bind(summarized).execute(&pool).await.unwrap();
        }
        sqlx::query("INSERT INTO toonflow.agent_memories(id,agent_type,isolation_key,role,content,memory_type,related_message_ids,create_time) VALUES(9200020,'scriptAgent',$1,'system','summary','summary',$2,9200020)")
            .bind(isolation).bind(json!([9_200_010_i64,9_200_011_i64])).execute(&pool).await.unwrap();

        let expanded = toonflow_agent_runtime::expand_related_messages(
            &pool,
            "scriptAgent",
            isolation,
            &[9_200_011, 9_200_010],
        )
        .await
        .unwrap();
        assert_eq!(expanded, vec!["first", "second"]);
        toonflow_agents::clear_memory_records(&pool, "scriptAgent", isolation, "summary")
            .await
            .unwrap();
        let summarized: Vec<bool> = sqlx::query_scalar(
            "SELECT summarized FROM toonflow.agent_memories WHERE isolation_key=$1 ORDER BY id",
        )
        .bind(isolation)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(summarized, vec![false, false]);

        let tokens = TokenService::new(
            SecurityConfig::new(
                "agent-memory-test-secret-at-least-32-bytes",
                "test",
                "test",
                Duration::from_secs(60),
            )
            .unwrap(),
        );
        let state = ToonState::new(pool.clone(), tokens);
        let first_version = format!(
            "1-1 客厅 日/内\n人物：甲\n△甲走进客厅。\n{}",
            "完整剧本第一版正文。".repeat(40)
        );
        let second_version = format!(
            "1-1 客厅 日/内\n人物：甲\n△甲走进客厅。\n{}",
            "完整剧本第二版正文。".repeat(40)
        );
        for content in [&first_version, &second_version] {
            toonflow_agent_tools::execute_inner(
                &state,
                &toonflow_agent_tools::ToolRequest {
                    agent_type: "scriptAgent".into(),
                    agent_key: Some("scriptAgent:decisionAgent".into()),
                    isolation_key: isolation.into(),
                    project_id,
                    script_id: None,
                    tool_name: "save_scripts".into(),
                    arguments: json!({"scripts":[{"name":"episode one","content":content}]}),
                    emitter: None,
                },
            )
            .await
            .unwrap();
        }
        let scripts: Vec<(String, String)> =
            sqlx::query_as("SELECT name,content FROM toonflow.scripts WHERE project_id=$1")
                .bind(project_id)
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            scripts,
            vec![("episode one".into(), second_version.clone())]
        );

        sqlx::query("INSERT INTO toonflow.agent_memories(id,agent_type,isolation_key,role,content,memory_type,related_message_ids,create_time) VALUES(9200021,'scriptAgent',$1,'system','summary two','summary',$2,9200021)")
            .bind(isolation).bind(json!([9_200_010_i64])).execute(&pool).await.unwrap();
        toonflow_agents::clear_memory_records(&pool, "scriptAgent", isolation, "message")
            .await
            .unwrap();
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM toonflow.agent_memories WHERE isolation_key=$1",
        )
        .bind(isolation)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(count, 0);
        sqlx::query("DELETE FROM toonflow.projects WHERE id=$1")
            .bind(project_id)
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn cleanup_protects_frozen_sources_for_every_nonterminal_job_state() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();

        let object_key = "projects/cleanup-reference-test/frozen-source.mp4";
        let cleanup_path = format!("/toonflow/assets/files/{object_key}");
        let source_paths = [
            ("queued", object_key.to_string()),
            ("retry", cleanup_path.clone()),
            (
                "running",
                format!("/api/toonflow/assets/files/{object_key}"),
            ),
        ];

        for (state, source_path) in source_paths {
            let task_id: i64 = sqlx::query_scalar(
                "INSERT INTO toonflow.tasks(
                   task_class,related_objects,model,description,state,start_time,input,
                   progress_current,progress_total
                 ) VALUES(
                   'cleanupReferenceTest','{}','toon.video_export',$1,'running',
                   (extract(epoch FROM clock_timestamp())*1000)::bigint,'{}'::jsonb,0,1
                 ) RETURNING id",
            )
            .bind(format!("cleanup source reference in {state}"))
            .fetch_one(&pool)
            .await
            .unwrap();
            let payload = json!({
                "sources": [{"videoId": 1, "filePath": source_path}],
            });
            sqlx::query(
                "INSERT INTO toonflow.distributed_jobs(
                   task_id,kind,payload,state,lease_owner,lease_token,lease_until
                 ) VALUES(
                   $1,'toon.video_export',$2,$3,
                   CASE WHEN $3='running' THEN 'cleanup-reference-test' END,
                   CASE WHEN $3='running' THEN gen_random_uuid() END,
                   CASE WHEN $3='running' THEN now()+interval '5 minutes' END
                 )",
            )
            .bind(task_id)
            .bind(payload)
            .bind(state)
            .execute(&pool)
            .await
            .unwrap();

            let mut tx = pool.begin().await.unwrap();
            assert_eq!(
                storage_object_reference(&mut tx, &cleanup_path)
                    .await
                    .unwrap(),
                StorageObjectReference::ActiveJob,
                "{state} source snapshot must protect its object-storage object"
            );
            tx.rollback().await.unwrap();

            sqlx::query(
                "UPDATE toonflow.distributed_jobs
                 SET state='succeeded',lease_owner=NULL,lease_token=NULL,lease_until=NULL,
                     completed_at=now()
                 WHERE task_id=$1",
            )
            .bind(task_id)
            .execute(&pool)
            .await
            .unwrap();
            let mut tx = pool.begin().await.unwrap();
            assert_eq!(
                storage_object_reference(&mut tx, &cleanup_path)
                    .await
                    .unwrap(),
                StorageObjectReference::None,
                "terminal {state} job must release its source snapshot"
            );
            tx.rollback().await.unwrap();
            sqlx::query("DELETE FROM toonflow.tasks WHERE id=$1")
                .bind(task_id)
                .execute(&pool)
                .await
                .unwrap();
        }

        let task_id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.tasks(
               task_class,related_objects,model,description,state,start_time,input,
               progress_current,progress_total
             ) VALUES(
               'cleanupReferenceTest','{}','toon.video_export','malformed sources','running',
               (extract(epoch FROM clock_timestamp())*1000)::bigint,'{}'::jsonb,0,1
             ) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.distributed_jobs(task_id,kind,payload,state)
             VALUES($1,'toon.video_export','{\"sources\":{}}'::jsonb,'queued')",
        )
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert_eq!(
            storage_object_reference(&mut tx, &cleanup_path)
                .await
                .unwrap(),
            StorageObjectReference::None,
            "malformed non-array sources must be ignored without failing cleanup"
        );
        tx.rollback().await.unwrap();
        sqlx::query("DELETE FROM toonflow.tasks WHERE id=$1")
            .bind(task_id)
            .execute(&pool)
            .await
            .unwrap();

        let transient_path = "cleanup-reference-test-transient-sentinel";
        let task_id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.tasks(
               task_class,related_objects,model,description,state,start_time,input,
               progress_current,progress_total
             ) VALUES(
               'cleanupReferenceTest','{}','toon.video_export','temporary source lifecycle','running',
               (extract(epoch FROM clock_timestamp())*1000)::bigint,'{}'::jsonb,0,1
             ) RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.distributed_jobs(task_id,kind,payload,state)
             VALUES($1,'toon.video_export',$2,'queued')",
        )
        .bind(task_id)
        .bind(json!({
            "sources": [{"videoId": 2, "filePath": transient_path}],
        }))
        .execute(&pool)
        .await
        .unwrap();
        let cleanup_id: i64 = sqlx::query_scalar(
            "INSERT INTO toonflow.storage_cleanup_tasks(
               object_path,resource_type,resource_id,error_reason,state,create_time,update_time
             ) VALUES(
               $1,'distributed_job_source',$2,'temporary source lifecycle','pending',
               (extract(epoch FROM clock_timestamp())*1000)::bigint,
               (extract(epoch FROM clock_timestamp())*1000)::bigint
             ) RETURNING id",
        )
        .bind(transient_path)
        .bind(task_id)
        .fetch_one(&pool)
        .await
        .unwrap();

        let first_cleanup = process_storage_cleanup_batch(
            &pool,
            "cleanup-reference-test",
            1,
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!((first_cleanup.completed, first_cleanup.deferred), (0, 1));
        let deferred: (String, String, bool) = sqlx::query_as(
            "SELECT state,error_reason,next_attempt_at > now()
             FROM toonflow.storage_cleanup_tasks WHERE id=$1",
        )
        .bind(cleanup_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(deferred.0, "pending");
        assert!(deferred.1.contains("临时引用"));
        assert!(deferred.2, "temporary references must be rechecked later");

        sqlx::query(
            "UPDATE toonflow.distributed_jobs
             SET state='succeeded',completed_at=now() WHERE task_id=$1",
        )
        .bind(task_id)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("UPDATE toonflow.storage_cleanup_tasks SET next_attempt_at=now() WHERE id=$1")
            .bind(cleanup_id)
            .execute(&pool)
            .await
            .unwrap();
        let terminal_cleanup = process_storage_cleanup_batch(
            &pool,
            "cleanup-reference-test",
            1,
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(
            (terminal_cleanup.completed, terminal_cleanup.deferred),
            (1, 0)
        );
        let cleanup_state: String =
            sqlx::query_scalar("SELECT state FROM toonflow.storage_cleanup_tasks WHERE id=$1")
                .bind(cleanup_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(cleanup_state, "completed");
        sqlx::query("DELETE FROM toonflow.tasks WHERE id=$1")
            .bind(task_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM toonflow.storage_cleanup_tasks WHERE id=$1")
            .bind(cleanup_id)
            .execute(&pool)
            .await
            .unwrap();
    }
}

impl ToonState {
    pub fn new(pool: PgPool, tokens: TokenService) -> Self {
        Self { pool, tokens }
    }
}

pub fn routes(state: ToonState) -> Router {
    let protected = Router::new()
        .route("/toon/projects", get(projects::list).post(projects::create))
        .route(
            "/toon/projects/{id}",
            get(projects::get)
                .put(projects::update)
                .delete(projects::delete),
        )
        .route("/toon/projects/{id}/publish", post(projects::publish))
        .route(
            "/toon/projects/{project_id}/episodes",
            get(episodes::list).post(episodes::create),
        )
        .route(
            "/toon/episodes/{id}",
            put(episodes::update).delete(episodes::delete),
        )
        .route(
            "/toon/episodes/{episode_id}/scenes",
            get(scenes::list).post(scenes::create),
        )
        .route(
            "/toon/scenes/{id}",
            put(scenes::update).delete(scenes::delete),
        )
        .route("/toonflow/health", get(toonflow_status::health))
        .route(
            "/toonflow/art-styles",
            get(toonflow_resources::list_art_styles).post(toonflow_resources::save_art_style),
        )
        .route(
            "/toonflow/art-styles/{id}",
            axum::routing::delete(toonflow_resources::delete_art_style),
        )
        .route("/toonflow/tasks", get(toonflow_resources::list_tasks))
        // The frontend/reverse proxy strips its /api prefix before forwarding.
        // Keep these aliases alongside the original /api/task compatibility API.
        .route("/task/getTaskApi", post(toonflow_resources::query_tasks))
        .route("/task/taskDetails", post(toonflow_resources::task_details))
        .route("/task/getTaskCategories", post(toonflow_resources::task_categories))
        .route("/task/getProject", post(toonflow_resources::task_projects))
        .route(
            "/toonflow/projects/{project_id}/video-archive",
            get(toonflow_episode_renders::project_video_archive),
        )
        .route(
            "/toonflow/projects/{project_id}/episodes/{script_id}/renders",
            get(toonflow_episode_renders::list_episode_renders),
        )
        .route(
            "/toonflow/episode-renders/{render_id}/current",
            axum::routing::patch(toonflow_episode_renders::select_current_render),
        )
        .route(
            "/toonflow/prompts",
            get(toonflow_resources::list_prompts).post(toonflow_resources::save_prompt),
        )
        .route(
            "/toonflow/prompts/{id}",
            axum::routing::delete(toonflow_resources::delete_prompt),
        )
        .route("/toonflow/skills", get(toonflow_resources::list_skills))
        .route(
            "/api/setting/skillManagement/getSkillList",
            post(toonflow_resources::skill_paths),
        )
        .route(
            "/api/setting/skillManagement/getSkillContent",
            post(toonflow_resources::skill_content),
        )
        .route(
            "/api/setting/skillManagement/saveSkillContent",
            post(toonflow_resources::save_skill_content),
        )
        .route("/setting/skillManagement/saveSkillContent", post(toonflow_resources::save_skill_content))
        .route(
            "/api/artStyle/getArtStyle",
            post(toonflow_resources::list_art_styles),
        )
        .route(
            "/api/artStyle/addArtStyle",
            post(toonflow_resources::save_art_style),
        )
        .route(
            "/api/artStyle/editArtStyle",
            post(toonflow_resources::save_art_style),
        )
        .route(
            "/api/task/getTaskApi",
            post(toonflow_resources::query_tasks),
        )
        .route(
            "/api/task/taskDetails",
            post(toonflow_resources::task_details),
        )
        .route(
            "/api/task/getTaskCategories",
            post(toonflow_resources::task_categories),
        )
        .route(
            "/api/task/getProject",
            post(toonflow_resources::task_projects),
        )
        .route(
            "/api/general/getSingleProject",
            post(toonflow_project::get_single_project),
        )
        .route(
            "/api/general/generalStatistics",
            post(toonflow_project::general_statistics),
        )
        .route(
            "/general/generalStatistics",
            post(toonflow_project::general_statistics),
        )
        .route(
            "/api/general/updateProject",
            post(toonflow_project::update_project_profile),
        )
        .route(
            "/general/updateProject",
            post(toonflow_project::update_project_profile),
        )
        .route(
            "/api/project/getModelDetails",
            post(toonflow_project::get_model_details),
        )
        .route(
            "/project/getModelDetails",
            post(toonflow_project::get_model_details),
        )
        .route("/toonflow/manuals", get(toonflow_manuals::list_all))
        .route(
            "/api/project/getVisualManual",
            post(toonflow_manuals::list_visual),
        )
        .route(
            "/api/project/queryDirectorManual",
            post(toonflow_manuals::list_director),
        )
        .route(
            "/api/project/addVisualManual",
            post(toonflow_manuals::save_visual),
        )
        .route("/project/addVisualManual", post(toonflow_manuals::save_visual))
        .route(
            "/api/project/editVisualManual",
            post(toonflow_manuals::save_visual),
        )
        .route("/project/editVisualManual", post(toonflow_manuals::save_visual))
        .route(
            "/api/project/addDirectorManual",
            post(toonflow_manuals::save_director),
        )
        .route("/project/addDirectorManual", post(toonflow_manuals::save_director))
        .route(
            "/api/project/editDirectorlManual",
            post(toonflow_manuals::save_director),
        )
        .route("/project/editDirectorlManual", post(toonflow_manuals::save_director))
        .route(
            "/api/project/deleteVisualManual",
            post(toonflow_manuals::delete_visual),
        )
        .route("/project/deleteVisualManual", post(toonflow_manuals::delete_visual))
        .route(
            "/api/project/deleteDirectorManual",
            post(toonflow_manuals::delete_director),
        )
        .route("/project/deleteDirectorManual", post(toonflow_manuals::delete_director))
        .route(
            "/api/novel/event/generateEvents",
            post(toonflow_novel_events::generate),
        )
        .route(
            "/api/novel/event/getEvent",
            post(toonflow_novel_events::list),
        )
        .route(
            "/api/novel/getNovelEventState",
            post(toonflow_novel_events::states),
        )
        .route(
            "/api/novel/event/deletEvent",
            post(toonflow_novel_events::delete),
        )
        .route(
            "/api/novel/event/batchDeleteEvent",
            post(toonflow_novel_events::batch_delete),
        )
        .route(
            "/api/assetsGenerate/polishAssetsPrompt",
            post(toonflow_asset_ai::polish),
        )
        .route(
            "/api/assetsGenerate/batchPolishAssetsPrompt",
            post(toonflow_asset_ai::batch_polish),
        )
        .route(
            "/api/assets/pollingPromptAssets",
            post(toonflow_asset_ai::poll_prompts),
        )
        .route(
            "/api/assetsGenerate/generateAssets",
            post(toonflow_asset_ai::generate_image),
        )
        .route(
            "/api/assetsGenerate/batchGenerateImageAssets",
            post(toonflow_asset_ai::batch_generate_images),
        )
        .route(
            "/api/production/assets/batchGenerateAssetsImage",
            post(toonflow_asset_ai::batch_generate_images),
        )
        .route(
            "/api/assetsGenerate/retryImageAssets",
            post(toonflow_asset_ai::retry_images),
        )
        .route(
            "/api/assets/pollingImageAssets",
            post(toonflow_asset_ai::poll_images),
        )
        .route(
            "/api/production/assets/pollingImage",
            post(toonflow_asset_ai::poll_images),
        )
        .route("/api/assets/getImage", post(toonflow_asset_ai::get_images))
        .route(
            "/api/assets/getMaterialData",
            post(toonflow_materials::list_materials),
        )
        .route(
            "/api/assets/uploadClip",
            post(toonflow_materials::upload_clip),
        )
        .route("/assets/uploadClip", post(toonflow_materials::upload_clip))
        .route(
            "/api/assets/delImage",
            post(toonflow_asset_ai::delete_image),
        )
        .route(
            "/api/assetsGenerate/cancelGenerate",
            post(toonflow_asset_ai::cancel_image),
        )
        .route(
            "/api/assets/batchGenerationData",
            post(toonflow_materials::asset_page),
        )
        .route(
            "/api/cornerScape/getAllAssets",
            post(toonflow_audio::all_assets),
        )
        .route(
            "/api/cornerScape/updateAssetsAudio",
            post(toonflow_audio::update_binding),
        )
        .route(
            "/api/cornerScape/batchBindAudio",
            post(toonflow_audio::batch_bind),
        )
        .route("/api/cornerScape/pollingAudio", post(toonflow_audio::poll))
        .route(
            "/api/cornerScape/generateDubbing",
            post(toonflow_audio::generate_dubbing),
        )
        .route("/api/agents/chat", post(toonflow_agents::chat))
        .route("/api/agents/start", post(toonflow_agents::start))
        .route("/api/agents/runState", post(toonflow_agents::run_state))
        .route("/api/agents/stop", post(toonflow_agents::stop))
        .route("/api/agents/events", post(toonflow_agents::events))
        .route("/api/agents/retry", post(toonflow_agents::retry))
        .route("/api/agents/memories", post(toonflow_agents::memories))
        .route(
            "/api/agents/getMemory",
            post(toonflow_agents::get_memory_compat),
        )
        .route("/api/agents/runs", post(toonflow_agents::runs))
        .route("/api/agents/clearMemory", post(toonflow_agents::clear))
        .route(
            "/api/agents/deleteAllMemory",
            post(toonflow_agents::clear_all),
        )
        .route(
            "/api/agents/tools/execute",
            post(toonflow_agent_tools::execute),
        )
        .route(
            "/api/scriptAgent/getPlanData",
            post(toonflow_agent_tools::get_plan),
        )
        .route(
            "/api/scriptAgent/setPlanData",
            post(toonflow_agent_tools::set_plan),
        )
        .route(
            "/api/scriptAgent/updateData",
            post(toonflow_agent_tools::update_plan),
        )
        .route(
            "/scriptAgent/updateData",
            post(toonflow_agent_tools::update_plan),
        )
        .route(
            "/script/extractAssets",
            post(toonflow_script_ai::extract_assets),
        )
        .route("/script/pollScriptAssets", post(toonflow_script_ai::poll))
        .route(
            "/api/script/extractAssets",
            post(toonflow_script_ai::extract_assets),
        )
        .route(
            "/api/script/pollScriptAssets",
            post(toonflow_script_ai::poll),
        )
        .route(
            "/api/production/editImage/getImageFlow",
            post(toonflow_image_workflow::get_flow),
        )
        .route(
            "/api/production/editImage/saveImageFlow",
            post(toonflow_image_workflow::save_flow),
        )
        .route(
            "/api/production/editImage/updateImageFlow",
            post(toonflow_image_workflow::update_flow),
        )
        .route(
            "/api/production/editImage/generateFlowImage",
            post(toonflow_image_workflow::generate_flow_image),
        )
        .route(
            "/api/production/editImage/getImageDefaultModle",
            post(toonflow_image_workflow::default_model),
        )
        .route(
            "/api/production/editImage/uploadImage",
            post(toonflow_materials::upload_flow_image),
        )
        .route(
            "/api/production/storyboard/batchGenerateImage",
            post(toonflow_image_workflow::generate_storyboards),
        )
        .route(
            "/api/production/storyboard/pollingImage",
            post(toonflow_image_workflow::poll_storyboards),
        )
        .route(
            "/api/production/storyboard/updateStoryboardUrl",
            post(toonflow_image_workflow::update_storyboard_url),
        )
        .route(
            "/api/production/storyboard/batchDelete",
            post(toonflow_image_workflow::delete_storyboards),
        )
        .route(
            "/api/production/storyboard/previewImage",
            post(toonflow_image_workflow::preview_storyboards),
        )
        .route(
            "/api/production/storyboard/downPreviewImage",
            post(toonflow_image_workflow::download_storyboards),
        )
        .route(
            "/api/production/sceneConsistency/catalog",
            post(toonflow_scene_consistency::list_scene_catalog),
        )
        .route(
            "/api/production/sceneConsistency/autoConfigure",
            post(toonflow_scene_consistency_auto::auto_configure_scene_consistency),
        )
        .route(
            "/api/production/sceneConsistency/saveMaster",
            post(toonflow_scene_consistency::save_scene_master),
        )
        .route(
            "/api/production/sceneConsistency/saveState",
            post(toonflow_scene_consistency::save_scene_state),
        )
        .route(
            "/api/production/workbench/addTrack",
            post(toonflow_video::add_track),
        )
        .route(
            "/api/production/workbench/deleteTrack",
            post(toonflow_video::delete_track),
        )
        .route(
            "/api/production/workbench/getVideoList",
            post(toonflow_video::video_list),
        )
        .route(
            "/api/production/workbench/updateVideoPrompt",
            post(toonflow_video::update_prompt),
        )
        .route(
            "/api/production/workbench/updateVideoDuration",
            post(toonflow_video::update_duration),
        )
        .route(
            "/api/production/workbench/updateVideoContinuityMode",
            post(toonflow_video::update_continuity_mode),
        )
        .route(
            "/api/production/workbench/updateVideoTransitionSettings",
            post(toonflow_video::update_transition_settings),
        )
        .route(
            "/api/production/workbench/selectVideo",
            post(toonflow_video::select_video),
        )
        .route(
            "/api/production/workbench/delVideo",
            post(toonflow_video::delete_video),
        )
        .route(
            "/api/production/workbench/getGenerateData",
            post(toonflow_video::generate_data),
        )
        .route(
            "/api/production/workbench/getAudioBindAssetsList",
            post(toonflow_video::audio_bind_assets),
        )
        .route(
            "/api/production/workbench/getFileUrl",
            post(toonflow_video::file_urls),
        )
        .route(
            "/api/production/workbench/generateVideo",
            post(toonflow_video::generate_video),
        )
        .route(
            "/api/production/workbench/checkVideoStateList",
            post(toonflow_video::check_states),
        )
        .route("/production/workbench/inspectVideo", post(toonflow_video_quality::inspect))
        .route("/api/production/workbench/inspectVideo", post(toonflow_video_quality::inspect))
        .route(
            "/api/production/workbench/generateVideoPrompt",
            post(toonflow_video::generate_prompt),
        )
        .route(
            "/api/production/workbench/checkVideoPrompt",
            post(toonflow_video::check_prompts),
        )
        .route(
            "/api/production/workbench/batchGeneratePrompt",
            post(toonflow_video::batch_prompts),
        )
        .route(
            "/api/production/workbench/batchGenerateVideo",
            post(toonflow_video::batch_videos),
        )
        .route(
            "/api/production/workbench/reorderTracks",
            post(toonflow_video::reorder_tracks),
        )
        .route(
            "/api/production/workbench/bindStoryboards",
            post(toonflow_video::bind_storyboards),
        )
        .route(
            "/api/production/workbench/cancelVideo",
            post(toonflow_video::cancel_video),
        )
        .route(
            "/api/production/workbench/retryVideo",
            post(toonflow_video::retry_video),
        )
        .route(
            "/api/production/workbench/exportVideo",
            post(toonflow_video_export::export),
        )
        .route(
            "/toonflow/projects",
            get(toonflow::list_projects).post(toonflow_project_crud::create_project),
        )
        .route(
            "/toonflow/projects/{id}",
            get(toonflow::get_project_by_path),
        )
        .route(
            "/toonflow/project/getProject",
            get(toonflow::list_projects).post(toonflow::list_projects),
        )
        .route(
            "/toonflow/project/addProject",
            post(toonflow_project_crud::create_project),
        )
        .route(
            "/toonflow/project/editProject",
            post(toonflow_project_crud::update_project),
        )
        .route(
            "/toonflow/project/delProject",
            post(toonflow_project_crud::delete_project),
        )
        .route("/toonflow/novel/addNovel", post(toonflow::add_novel))
        .route("/toonflow/novel/getNovel", post(toonflow::list_novel))
        .route("/toonflow/novel/getNovelData", post(toonflow::all_novel))
        .route("/toonflow/novel/updateNovel", post(toonflow::update_novel))
        .route("/toonflow/novel/delNovel", post(toonflow::delete_novel))
        .route("/toonflow/script/addScript", post(toonflow::add_script))
        .route(
            "/toonflow/script/batchAddScript",
            post(toonflow::batch_add_scripts),
        )
        .route("/toonflow/script/getScrptApi", post(toonflow::list_scripts))
        .route(
            "/toonflow/script/updateScript",
            post(toonflow::update_script),
        )
        .route("/toonflow/script/delScript", post(toonflow::delete_scripts))
        .route("/toonflow/assets/getAssetsApi", post(toonflow::list_assets))
        .route(
            "/toonflow/assets/library",
            post(toonflow_asset_library::list),
        )
        .route("/toonflow/assets/link", post(toonflow_asset_library::link))
        .route(
            "/toonflow/assets/unlink",
            post(toonflow_asset_library::unlink),
        )
        .route("/toonflow/assets/saveAssets", post(toonflow::save_asset))
        .route("/toonflow/assets/addAssets", post(toonflow::save_asset))
        .route("/toonflow/assets/updateAssets", post(toonflow::save_asset))
        .route(
            "/toonflow/assets/batchDelete",
            post(toonflow::delete_assets),
        )
        .route(
            "/toonflow/production/getFlowData",
            post(toonflow::get_flow_data),
        )
        .route(
            "/toonflow/production/saveFlowData",
            post(toonflow::save_flow_data),
        )
        .route(
            "/toonflow/production/validateWorkflow",
            post(toonflow_workflow::validate),
        )
        .route(
            "/toonflow/production/workflowRuns",
            get(toonflow_workflow::list_runs).post(toonflow_workflow::create_run),
        )
        .route(
            "/toonflow/production/workflowRuns/state",
            post(toonflow_workflow::run_state),
        )
        .route(
            "/toonflow/production/workflowRuns/cancel",
            post(toonflow_workflow::cancel_run),
        )
        .route(
            "/toonflow/production/workflowNodeRuns/start",
            post(toonflow_workflow::start_node),
        )
        .route(
            "/toonflow/production/workflowNodeRuns/state",
            post(toonflow_workflow::node_state),
        )
        .route(
            "/toonflow/production/workflowNodeRuns/latest",
            get(toonflow_workflow::latest_node_run),
        )
        .route(
            "/toonflow/production/workflowNodeRuns/cancel",
            post(toonflow_workflow::cancel_node),
        )
        .route(
            "/toonflow/production/workflowNodeRuns/retry",
            post(toonflow_workflow::retry_node),
        )
        .route(
            "/toonflow/production/getStoryboardData",
            post(toonflow::get_storyboards),
        )
        .route(
            "/toonflow/production/storyboard/addStoryboard",
            post(toonflow::add_storyboard),
        )
        .route(
            "/toonflow/production/storyboard/batchAddStoryboardInfo",
            post(toonflow::batch_add_storyboards),
        )
        .route(
            "/toonflow/production/storyboard/editStoryboardInfo",
            post(toonflow::edit_storyboard_info),
        )
        .route(
            "/toonflow/production/storyboard/removeFrame",
            post(toonflow::remove_storyboard),
        )
        .route(
            "/toonflow/production/storyboard/reorder",
            post(toonflow::reorder_storyboards),
        )
        .route(
            "/toonflow/production/sceneConsistency/catalog",
            post(toonflow_scene_consistency::list_scene_catalog),
        )
        .route(
            "/toonflow/production/sceneConsistency/autoConfigure",
            post(toonflow_scene_consistency_auto::auto_configure_scene_consistency),
        )
        .route(
            "/toonflow/production/sceneConsistency/saveMaster",
            post(toonflow_scene_consistency::save_scene_master),
        )
        .route(
            "/toonflow/production/sceneConsistency/saveState",
            post(toonflow_scene_consistency::save_scene_state),
        )
        .route(
            "/toonflow/setting/agentDeploy",
            get(toonflow::list_agent_deployments).post(toonflow::update_agent_deployment),
        )
        .route(
            "/toonflow/setting/settings",
            get(toonflow::list_settings).post(toonflow::save_setting),
        )
        .route(
            "/toonflow/setting/getAgentUseMode",
            get(toonflow::get_agent_use_mode),
        )
        .route(
            "/toonflow/setting/updateAgentUseMode",
            post(toonflow::update_agent_use_mode),
        )
        .route("/api/project/getProject", post(toonflow::list_projects))
        .route(
            "/api/project/addProject",
            post(toonflow_project_crud::create_project),
        )
        .route(
            "/api/project/editProject",
            post(toonflow_project_crud::update_project),
        )
        .route(
            "/api/project/delProject",
            post(toonflow_project_crud::delete_project),
        )
        .route("/api/novel/addNovel", post(toonflow::add_novel))
        .route("/api/novel/getNovel", post(toonflow::list_novel))
        .route("/api/novel/getNovelData", post(toonflow::all_novel))
        .route("/api/novel/updateNovel", post(toonflow::update_novel))
        .route("/api/novel/delNovel", post(toonflow::delete_novel))
        .route("/api/novel/getNovelIndex", post(toonflow::novel_index))
        .route(
            "/api/novel/batchDeleteNovel",
            post(toonflow::batch_delete_novel),
        )
        .route("/api/script/addScript", post(toonflow::add_script))
        .route(
            "/api/script/batchAddScript",
            post(toonflow::batch_add_scripts),
        )
        .route("/script/batchAddScript", post(toonflow::batch_add_scripts))
        .route("/api/script/getScrptApi", post(toonflow::list_scripts))
        .route("/api/script/updateScript", post(toonflow::update_script))
        .route("/api/script/delScript", post(toonflow::delete_scripts))
        .route("/api/script/getAiRegex", post(toonflow_script_ai::ai_regex))
        .route(
            "/api/script/polishScriptPrompt",
            post(toonflow_script_ai::polish_script_prompt),
        )
        .route(
            "/api/script/exportScript",
            post(toonflow_script_ai::export_scripts),
        )
        .route(
            "/api/assets/getAssetsApi",
            post(toonflow::list_assets_compat),
        )
        .route("/api/assets/saveAssets", post(toonflow::save_asset))
        .route("/api/assets/addAssets", post(toonflow::save_asset))
        .route("/api/assets/updateAssets", post(toonflow::save_asset))
        .route("/api/assets/batchDelete", post(toonflow::delete_assets))
        .route("/api/assets/delAssets", post(toonflow::delete_asset))
        .route(
            "/api/assets/addAudioAssets",
            post(toonflow_audio::add_audio_assets),
        )
        .route(
            "/api/assets/updateAudioAssets",
            post(toonflow_audio::update_audio_assets),
        )
        .route(
            "/api/artStyle/extractStylePrompt",
            post(toonflow_resources::extract_style_prompt),
        )
        .route(
            "/api/production/assets/updateAssetsUrl",
            post(toonflow_image_workflow::update_asset_url),
        )
        .route(
            "/api/production/assets/deleteAssetsDireve",
            post(toonflow_image_workflow::delete_derived_asset),
        )
        .route(
            "/api/modelSelect/getModelList",
            post(toonflow_resources::model_list),
        )
        .route(
            "/api/modelSelect/getModelDetail",
            post(toonflow_resources::model_detail),
        )
        .route("/api/other/getVersion", get(toonflow_resources::version))
        .route("/api/production/getFlowData", post(toonflow::get_flow_data))
        .route(
            "/api/production/saveFlowData",
            post(toonflow::save_flow_data),
        )
        .route(
            "/api/production/getStoryboardData",
            post(toonflow::get_storyboards),
        )
        .route(
            "/api/production/storyboard/addStoryboard",
            post(toonflow::add_storyboard),
        )
        .route(
            "/api/production/storyboard/batchAddStoryboardInfo",
            post(toonflow::batch_add_storyboards),
        )
        .route(
            "/api/production/storyboard/editStoryboardInfo",
            post(toonflow::edit_storyboard_info),
        )
        .route(
            "/api/production/storyboard/removeFrame",
            post(toonflow::remove_storyboard),
        )
        .route(
            "/api/production/storyboard/reorder",
            post(toonflow::reorder_storyboards),
        )
        .route("/assets/getImage", post(toonflow_asset_ai::get_images))
        .route("/cornerScape/pollingAudio", post(toonflow_audio::poll))
        .route("/agents/chat", post(toonflow_agents::chat))
        .route("/agents/start", post(toonflow_agents::start))
        .route("/agents/runState", post(toonflow_agents::run_state))
        .route("/agents/stop", post(toonflow_agents::stop))
        .route("/agents/events", post(toonflow_agents::events))
        .route("/agents/retry", post(toonflow_agents::retry))
        .route("/agents/memories", post(toonflow_agents::memories))
        .route(
            "/agents/getMemory",
            post(toonflow_agents::get_memory_compat),
        )
        .route("/agents/runs", post(toonflow_agents::runs))
        .route("/agents/clearMemory", post(toonflow_agents::clear))
        .route("/agents/deleteAllMemory", post(toonflow_agents::clear_all))
        .route("/project/getProject", post(toonflow::list_projects))
        .route(
            "/project/addProject",
            post(toonflow_project_crud::create_project),
        )
        .route(
            "/project/editProject",
            post(toonflow_project_crud::update_project),
        )
        .route(
            "/project/delProject",
            post(toonflow_project_crud::delete_project),
        )
        .route("/novel/addNovel", post(toonflow::add_novel))
        .route("/novel/getNovel", post(toonflow::list_novel))
        .route("/novel/getNovelData", post(toonflow::all_novel))
        .route("/novel/updateNovel", post(toonflow::update_novel))
        .route("/novel/delNovel", post(toonflow::delete_novel))
        .route("/novel/getNovelIndex", post(toonflow::novel_index))
        .route(
            "/novel/batchDeleteNovel",
            post(toonflow::batch_delete_novel),
        )
        .route(
            "/novel/event/generateEvents",
            post(toonflow_novel_events::generate),
        )
        .route("/novel/event/getEvent", post(toonflow_novel_events::list))
        .route(
            "/novel/getNovelEventState",
            post(toonflow_novel_events::states),
        )
        .route(
            "/novel/event/deletEvent",
            post(toonflow_novel_events::delete),
        )
        .route(
            "/novel/event/batchDeleteEvent",
            post(toonflow_novel_events::batch_delete),
        )
        .route("/script/addScript", post(toonflow::add_script))
        .route("/script/getScrptApi", post(toonflow::list_scripts))
        .route("/script/updateScript", post(toonflow::update_script))
        .route("/script/delScript", post(toonflow::delete_scripts))
        .route("/script/getAiRegex", post(toonflow_script_ai::ai_regex))
        .route(
            "/script/polishScriptPrompt",
            post(toonflow_script_ai::polish_script_prompt),
        )
        .route(
            "/script/exportScript",
            post(toonflow_script_ai::export_scripts),
        )
        .route("/assets/getAssetsApi", post(toonflow::list_assets_compat))
        .route("/assets/saveAssets", post(toonflow::save_asset))
        .route("/assets/addAssets", post(toonflow::save_asset))
        .route("/assets/updateAssets", post(toonflow::save_asset))
        .route("/assets/batchDelete", post(toonflow::delete_assets))
        .route("/assets/delAssets", post(toonflow::delete_asset))
        .route(
            "/assets/addAudioAssets",
            post(toonflow_audio::add_audio_assets),
        )
        .route(
            "/assets/updateAudioAssets",
            post(toonflow_audio::update_audio_assets),
        )
        .route(
            "/artStyle/extractStylePrompt",
            post(toonflow_resources::extract_style_prompt),
        )
        .route(
            "/production/assets/updateAssetsUrl",
            post(toonflow_image_workflow::update_asset_url),
        )
        .route(
            "/production/assets/deleteAssetsDireve",
            post(toonflow_image_workflow::delete_derived_asset),
        )
        .route(
            "/modelSelect/getModelList",
            post(toonflow_resources::model_list),
        )
        .route(
            "/modelSelect/getModelDetail",
            post(toonflow_resources::model_detail),
        )
        .route("/other/getVersion", get(toonflow_resources::version))
        .route("/production/getFlowData", post(toonflow::get_flow_data))
        .route("/agents/tools/execute", post(toonflow_agent_tools::execute))
        .route(
            "/assets/getMaterialData",
            post(toonflow_materials::list_materials),
        )
        .route(
            "/assetsGenerate/generateAssets",
            post(toonflow_asset_ai::generate_image),
        )
        .route(
            "/assetsGenerate/batchGenerateImageAssets",
            post(toonflow_asset_ai::batch_generate_images),
        )
        .route(
            "/production/assets/batchGenerateAssetsImage",
            post(toonflow_asset_ai::batch_generate_images),
        )
        .route(
            "/assetsGenerate/retryImageAssets",
            post(toonflow_asset_ai::retry_images),
        )
        .route(
            "/assets/pollingImageAssets",
            post(toonflow_asset_ai::poll_images),
        )
        .route(
            "/production/assets/pollingImage",
            post(toonflow_asset_ai::poll_images),
        )
        .route(
            "/assetsGenerate/cancelGenerate",
            post(toonflow_asset_ai::cancel_image),
        )
        .route(
            "/assetsGenerate/polishAssetsPrompt",
            post(toonflow_asset_ai::polish),
        )
        .route(
            "/cornerScape/getAllAssets",
            post(toonflow_audio::all_assets),
        )
        .route(
            "/cornerScape/updateAssetsAudio",
            post(toonflow_audio::update_binding),
        )
        .route(
            "/cornerScape/batchBindAudio",
            post(toonflow_audio::batch_bind),
        )
        .route(
            "/cornerScape/generateDubbing",
            post(toonflow_audio::generate_dubbing),
        )
        .route(
            "/production/editImage/generateFlowImage",
            post(toonflow_image_workflow::generate_flow_image),
        )
        .route(
            "/production/editImage/getImageFlow",
            post(toonflow_image_workflow::get_flow),
        )
        .route(
            "/production/editImage/saveImageFlow",
            post(toonflow_image_workflow::save_flow),
        )
        .route(
            "/production/editImage/updateImageFlow",
            post(toonflow_image_workflow::update_flow),
        )
        .route(
            "/production/editImage/uploadImage",
            post(toonflow_materials::upload_flow_image),
        )
        .route(
            "/production/storyboard/batchGenerateImage",
            post(toonflow_image_workflow::generate_storyboards),
        )
        .route(
            "/production/storyboard/pollingImage",
            post(toonflow_image_workflow::poll_storyboards),
        )
        .route(
            "/production/storyboard/updateStoryboardUrl",
            post(toonflow_image_workflow::update_storyboard_url),
        )
        .route(
            "/production/storyboard/batchDelete",
            post(toonflow_image_workflow::delete_storyboards),
        )
        .route(
            "/production/storyboard/previewImage",
            post(toonflow_image_workflow::preview_storyboards),
        )
        .route(
            "/production/storyboard/downPreviewImage",
            post(toonflow_image_workflow::download_storyboards),
        )
        .route(
            "/production/sceneConsistency/catalog",
            post(toonflow_scene_consistency::list_scene_catalog),
        )
        .route(
            "/production/sceneConsistency/autoConfigure",
            post(toonflow_scene_consistency_auto::auto_configure_scene_consistency),
        )
        .route(
            "/production/sceneConsistency/saveMaster",
            post(toonflow_scene_consistency::save_scene_master),
        )
        .route(
            "/production/sceneConsistency/saveState",
            post(toonflow_scene_consistency::save_scene_state),
        )
        .route(
            "/production/workbench/addTrack",
            post(toonflow_video::add_track),
        )
        .route(
            "/production/workbench/deleteTrack",
            post(toonflow_video::delete_track),
        )
        .route(
            "/production/workbench/getVideoList",
            post(toonflow_video::video_list),
        )
        .route(
            "/production/workbench/updateVideoPrompt",
            post(toonflow_video::update_prompt),
        )
        .route(
            "/production/workbench/updateVideoDuration",
            post(toonflow_video::update_duration),
        )
        .route(
            "/production/workbench/updateVideoContinuityMode",
            post(toonflow_video::update_continuity_mode),
        )
        .route(
            "/production/workbench/updateVideoTransitionSettings",
            post(toonflow_video::update_transition_settings),
        )
        .route(
            "/production/workbench/selectVideo",
            post(toonflow_video::select_video),
        )
        .route(
            "/production/workbench/delVideo",
            post(toonflow_video::delete_video),
        )
        .route(
            "/production/workbench/getGenerateData",
            post(toonflow_video::generate_data),
        )
        .route(
            "/production/workbench/getFileUrl",
            post(toonflow_video::file_urls),
        )
        .route(
            "/production/workbench/exportVideo",
            post(toonflow_video_export::export),
        )
        .route(
            "/production/workbench/generateVideo",
            post(toonflow_video::generate_video),
        )
        .route(
            "/production/workbench/checkVideoStateList",
            post(toonflow_video::check_states),
        )
        .route(
            "/production/workbench/generateVideoPrompt",
            post(toonflow_video::generate_prompt),
        )
        .route(
            "/production/workbench/checkVideoPrompt",
            post(toonflow_video::check_prompts),
        )
        .route(
            "/production/workbench/batchGeneratePrompt",
            post(toonflow_video::batch_prompts),
        )
        .route(
            "/production/workbench/batchGenerateVideo",
            post(toonflow_video::batch_videos),
        )
        .route(
            "/production/workbench/reorderTracks",
            post(toonflow_video::reorder_tracks),
        )
        .route(
            "/production/workbench/bindStoryboards",
            post(toonflow_video::bind_storyboards),
        )
        .route(
            "/production/workbench/cancelVideo",
            post(toonflow_video::cancel_video),
        )
        .route(
            "/production/workbench/getAudioBindAssetsList",
            post(toonflow_video::audio_bind_assets),
        )
        .route(
            "/production/workbench/retryVideo",
            post(toonflow_video::retry_video),
        )
        .route(
            "/scriptAgent/getPlanData",
            post(toonflow_agent_tools::get_plan),
        )
        .route(
            "/scriptAgent/setPlanData",
            post(toonflow_agent_tools::set_plan),
        )
        .route(
            "/setting/skillManagement/getSkillContent",
            post(toonflow_resources::skill_content),
        )
        .route_layer(from_fn_with_state(state.tokens.clone(), authenticate));

    Router::new()
        .route("/toon/capabilities", get(capabilities))
        .route(
            "/toonflow/assets/files/{*key}",
            get(toonflow_storage::serve_image),
        )
        .route("/api/socket/{agent}", get(toonflow_ws::ws_handler))
        .route("/socket/{agent}", get(toonflow_ws::ws_handler))
        .merge(protected)
        .with_state(state)
}

async fn capabilities() -> Json<ApiResponse<ToonCapability>> {
    Json(ApiResponse::new(ToonCapability::default()))
}

#[cfg(test)]
mod proxy_route_tests {
    use super::*;
    use rust_toon_framework_security::SecurityConfig;
    use std::time::Duration;

    #[tokio::test]
    async fn prefixed_and_proxy_stripped_routes_both_reach_authentication() {
        // No database or user data is needed: a registered protected route must
        // return 401 for these requests, not fall through to 404.
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused").unwrap();
        let tokens = TokenService::new(SecurityConfig::new(
            "route-test-secret-with-at-least-32-bytes", "test", "test", Duration::from_secs(60),
        ).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = routes(ToonState::new(pool, tokens));
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let mut results = Vec::new();
        for path in [
            "/task/getTaskApi", "/task/taskDetails", "/task/getTaskCategories", "/task/getProject",
            "/setting/skillManagement/saveSkillContent", "/assets/uploadClip",
            "/project/editVisualManual", "/project/addVisualManual", "/project/editDirectorlManual",
            "/project/addDirectorManual", "/project/deleteVisualManual", "/project/deleteDirectorManual",
            "/production/workbench/inspectVideo",
        ] {
            for prefix in ["", "/api"] {
                let url = format!("http://{address}{prefix}{path}");
                let status = client.post(&url).json(&serde_json::json!({})).send().await.unwrap().status();
                results.push((url, status));
            }
        }
        server.abort();
        for (url, status) in results { assert_eq!(status, reqwest::StatusCode::UNAUTHORIZED, "{url}"); }
    }
}
