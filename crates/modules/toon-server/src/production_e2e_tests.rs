use std::{process::Command, time::Duration};

use axum::{
    Json,
    body::to_bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
use rust_toon_framework_security::{
    CurrentUser, DataScope, Permission, PermissionSet, SecurityConfig, TokenService,
};

use crate::{
    ToonState, toonflow, toonflow_episode_renders, toonflow_project_crud, toonflow_storage,
    toonflow_video, toonflow_video_export, toonflow_workflow,
};

fn user() -> CurrentUser {
    CurrentUser {
        user_id: "00000000-0000-0000-0000-000000000001".into(),
        username: "production-e2e".into(),
        tenant_id: None,
        role_codes: vec!["super_admin".into()],
        permissions: PermissionSet::default(),
        data_scope: DataScope::All,
    }
}

fn scoped_user(user_id: &str) -> CurrentUser {
    CurrentUser {
        user_id: user_id.into(),
        username: "production-e2e-owner".into(),
        tenant_id: None,
        role_codes: vec![],
        permissions: PermissionSet::new([
            Permission::new("toon:project:read").unwrap(),
            Permission::new("toon:episode:read").unwrap(),
            Permission::new("toon:episode:update").unwrap(),
            Permission::new("toon:scene:read").unwrap(),
            Permission::new("toon:scene:update").unwrap(),
            Permission::new("toon:scene:delete").unwrap(),
        ]),
        data_scope: DataScope::SelfOnly,
    }
}

async fn wait_for_export(pool: &sqlx::PgPool, task_id: i64) -> serde_json::Value {
    for _ in 0..200 {
        let task: Option<(String, String, Option<String>)> =
            sqlx::query_as("SELECT state,related_objects,reason FROM toonflow.tasks WHERE id=$1")
                .bind(task_id)
                .fetch_optional(pool)
                .await
                .expect("poll export task");
        if let Some((task_state, related_objects, reason)) = task {
            if task_state == "failed" {
                panic!("video export failed: {}", reason.unwrap_or(related_objects));
            }
            if task_state == "success" {
                return serde_json::from_str(&related_objects).expect("export task result json");
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("export task {task_id} did not complete");
}

#[tokio::test]
#[ignore = "run with script/test-production-e2e.sh"]
async fn project_content_storyboard_and_video_export_form_a_complete_pipeline() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let upload_dir = std::env::var("TEST_UPLOAD_DIR").expect("TEST_UPLOAD_DIR is required");
    // This ignored test runs in its own process through test-production-e2e.sh.

    let pool =
        connect(&DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).expect("database config"))
            .await
            .expect("database connection");
    migrate(&pool).await.expect("database migrations");
    let tokens = TokenService::new(
        SecurityConfig::new(
            "production-e2e-secret-at-least-32-bytes",
            "test",
            "test",
            Duration::from_secs(60),
        )
        .expect("security config"),
    );
    let state = ToonState::new(pool.clone(), tokens);

    let project = toonflow_project_crud::create_project(
        user(),
        State(state.clone()),
        Json(toonflow::SaveProjectRequest {
            id: None,
            project_type: "shortDrama".into(),
            chat_model: None,
            image_model: None,
            image_quality: "2K".into(),
            video_model: None,
            name: "Production E2E".into(),
            intro: "automated production pipeline".into(),
            r#type: "anime".into(),
            art_style: "2D".into(),
            director_manual: String::new(),
            mode: "firstLastFrame".into(),
            video_ratio: "16:9".into(),
        }),
    )
    .await
    .expect("create project");
    let project_id = project.0.data["id"].as_i64().expect("project id");

    let _ = toonflow::add_novel(
        user(),
        State(state.clone()),
        Json(toonflow::AddNovelRequest {
            project_id,
            data: vec![toonflow::NovelItemRequest {
                index: 1,
                reel: "第一卷".into(),
                chapter: "第一章".into(),
                chapter_data: "主角在雨夜车站相遇。".into(),
            }],
        }),
    )
    .await
    .expect("add novel");

    let script = toonflow::add_script(
        user(),
        State(state.clone()),
        Json(toonflow::SaveScriptRequest {
            id: None,
            name: "第1集".into(),
            content: "雨夜车站，主角回头。".into(),
            project_id: Some(project_id),
            assets: Some(vec![]),
        }),
    )
    .await
    .expect("add script");
    let script_id = script.0.data["id"].as_i64().expect("script id");

    let outsider = scoped_user("00000000-0000-0000-0000-000000000002");
    let outsider_flow = toonflow::get_flow_data(
        outsider.clone(),
        State(state.clone()),
        Json(toonflow::FlowRequest {
            project_id,
            episodes_id: script_id,
        }),
    )
    .await
    .expect_err("another user must not read production flow data");
    assert_eq!(outsider_flow.status(), StatusCode::NOT_FOUND);
    let flow_data = toonflow::get_flow_data(
        user(),
        State(state.clone()),
        Json(toonflow::FlowRequest {
            project_id,
            episodes_id: script_id,
        }),
    )
    .await
    .expect("load default production flow")
    .0
    .data;
    let outsider_flow_write = toonflow::save_flow_data(
        outsider.clone(),
        State(state.clone()),
        Json(toonflow::SaveFlowRequest {
            project_id,
            episodes_id: script_id,
            data: flow_data.clone(),
        }),
    )
    .await
    .expect_err("another user must not overwrite production flow data");
    assert_eq!(outsider_flow_write.status(), StatusCode::NOT_FOUND);
    let _ = toonflow::save_flow_data(
        user(),
        State(state.clone()),
        Json(toonflow::SaveFlowRequest {
            project_id,
            episodes_id: script_id,
            data: flow_data,
        }),
    )
    .await
    .expect("persist production workflow");
    let workflow_run = toonflow_workflow::create_run(
        user(),
        State(state.clone()),
        Json(toonflow_workflow::CreateWorkflowRunRequest {
            project_id,
            script_id,
            trigger_type: "manual".into(),
            input: serde_json::json!({}),
            auto_start: false,
        }),
    )
    .await
    .expect("create pending workflow run")
    .0
    .data;
    let workflow_node_id: String = sqlx::query_scalar(
        "SELECT node_id FROM toonflow.workflow_node_runs
         WHERE workflow_run_id=$1 AND node_type='script.source' ORDER BY id LIMIT 1",
    )
    .bind(workflow_run.id)
    .fetch_one(&pool)
    .await
    .expect("load pending workflow node");
    let outsider_node_start = toonflow_workflow::start_node(
        outsider.clone(),
        State(state.clone()),
        Json(toonflow_workflow::StartWorkflowNodeRequest {
            workflow_run_id: workflow_run.id,
            node_id: workflow_node_id.clone(),
            input: serde_json::json!({}),
            agent_run_id: None,
        }),
    )
    .await
    .expect_err("another user must not start a project workflow node");
    assert_eq!(outsider_node_start.status(), StatusCode::NOT_FOUND);
    let outsider_run_read = toonflow_workflow::run_state(
        outsider.clone(),
        State(state.clone()),
        Json(toonflow_workflow::WorkflowRunIdRequest {
            id: workflow_run.id,
        }),
    )
    .await
    .expect_err("another user must not read a project workflow run");
    assert_eq!(outsider_run_read.status(), StatusCode::NOT_FOUND);
    let outsider_run_cancel = toonflow_workflow::cancel_run(
        outsider.clone(),
        State(state.clone()),
        Json(toonflow_workflow::WorkflowRunIdRequest {
            id: workflow_run.id,
        }),
    )
    .await
    .expect_err("another user must not cancel a project workflow run");
    assert_eq!(outsider_run_cancel.status(), StatusCode::NOT_FOUND);
    let (first_start, second_start) = tokio::join!(
        toonflow_workflow::start_node(
            user(),
            State(state.clone()),
            Json(toonflow_workflow::StartWorkflowNodeRequest {
                workflow_run_id: workflow_run.id,
                node_id: workflow_node_id.clone(),
                input: serde_json::json!({}),
                agent_run_id: None,
            }),
        ),
        toonflow_workflow::start_node(
            user(),
            State(state.clone()),
            Json(toonflow_workflow::StartWorkflowNodeRequest {
                workflow_run_id: workflow_run.id,
                node_id: workflow_node_id,
                input: serde_json::json!({}),
                agent_run_id: None,
            }),
        )
    );
    assert_eq!(
        usize::from(first_start.is_ok()) + usize::from(second_start.is_ok()),
        1,
        "a workflow node CAS claim must allow exactly one concurrent starter"
    );
    let _ = toonflow_workflow::cancel_run(
        user(),
        State(state.clone()),
        Json(toonflow_workflow::WorkflowRunIdRequest {
            id: workflow_run.id,
        }),
    )
    .await
    .expect("cancel workflow isolation fixture");

    let asset = toonflow::save_asset(
        user(),
        State(state.clone()),
        Json(toonflow::SaveAssetRequest {
            id: None,
            project_id,
            name: "雨夜车站".into(),
            prompt: Some("wet railway platform at night".into()),
            remark: None,
            r#type: Some("scene".into()),
            description: Some("雨水反射站台灯光".into()),
            script_id: Some(script_id),
            parent_asset_id: None,
            image_id: None,
            base64: None,
        }),
    )
    .await
    .expect("save asset");
    let asset_id = asset.0.data["id"].as_i64().expect("asset id");

    let _ = toonflow::batch_add_storyboards(
        user(),
        State(state.clone()),
        Json(toonflow::BatchStoryboardRequest {
            project_id,
            script_id,
            data: vec![toonflow::SaveStoryboardRequest {
                id: None,
                prompt: "@图1 的雨夜车站笼罩雨幕，主角站在站台中景".into(),
                duration: Some(1),
                state: "未生成".into(),
                video_desc: Some("镜头缓慢推进".into()),
                shot_size: None,
                camera_move: None,
                time_of_day: None,
                scene_key: None,
                scene_state_id: None,
                scene_state_key: None,
                scene_state_parent_key: None,
                scene_state_description: None,
                should_generate_image: 1,
                file_path: None,
                script_id: Some(script_id),
                project_id: Some(project_id),
                track: Some("main".into()),
                associate_assets_ids: vec![asset_id],
            }],
        }),
    )
    .await
    .expect("add storyboard");

    let track_id: i64 = sqlx::query_scalar(
        "SELECT id FROM toonflow.video_tracks WHERE project_id=$1 AND script_id=$2",
    )
    .bind(project_id)
    .bind(script_id)
    .fetch_one(&pool)
    .await
    .expect("storyboard track");

    let source_dir = std::path::Path::new(&upload_dir).join("e2e");
    std::fs::create_dir_all(&source_dir).expect("create upload directory");
    let source_path = source_dir.join("source.mp4");
    let output = Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=c=black:s=320x180:d=1",
            "-pix_fmt",
            "yuv420p",
        ])
        .arg(&source_path)
        .output()
        .expect("run ffmpeg fixture generation");
    assert!(output.status.success(), "ffmpeg fixture generation failed");
    let source_bytes = tokio::fs::read(&source_path)
        .await
        .expect("read source video");
    let source_url = toonflow_storage::persist_asset_bytes(project_id, "e2e", "mp4", source_bytes)
        .await
        .expect("persist source video");

    let video_id = project_id + 90_000;
    sqlx::query("INSERT INTO toonflow.videos(id,file_path,state,script_id,project_id,video_track_id) VALUES($1,$2,'生成成功',$3,$4,$5)")
        .bind(video_id)
        .bind(&source_url)
        .bind(script_id)
        .bind(project_id)
        .bind(track_id)
    .execute(&pool)
    .await
    .expect("insert selected video");
    let outsider_video_list = toonflow_video::video_list(
        outsider.clone(),
        State(state.clone()),
        Json(
            serde_json::from_value(serde_json::json!({
                "projectId": project_id,
                "scriptId": script_id
            }))
            .expect("video list request"),
        ),
    )
    .await
    .expect_err("another user must not list project videos");
    assert_eq!(outsider_video_list.status(), StatusCode::NOT_FOUND);
    let outsider_video_cancel = toonflow_video::cancel_video(
        outsider.clone(),
        State(state.clone()),
        Json(toonflow_video::Id { id: video_id }),
    )
    .await
    .expect_err("another user must not cancel a project video");
    assert_eq!(outsider_video_cancel.status(), StatusCode::NOT_FOUND);
    let outsider_video_delete = toonflow_video::delete_video(
        outsider.clone(),
        State(state.clone()),
        Json(toonflow_video::Id { id: video_id }),
    )
    .await
    .expect_err("another user must not delete a project video");
    assert_eq!(outsider_video_delete.status(), StatusCode::NOT_FOUND);
    let video_still_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM toonflow.videos WHERE id=$1 AND state='生成成功')",
    )
    .bind(video_id)
    .fetch_one(&pool)
    .await
    .expect("inspect protected video");
    assert!(video_still_exists);
    let outsider_generate_request = serde_json::from_value(serde_json::json!({
        "projectId": project_id,
        "scriptId": script_id,
        "prompt": "unauthorized",
        "model": "1",
        "mode": "text",
        "resolution": "720p",
        "duration": 1,
        "trackId": track_id,
        "uploadData": []
    }))
    .expect("outsider video generation request");
    let outsider_generate = toonflow_video::generate_video(
        outsider.clone(),
        State(state.clone()),
        Json(outsider_generate_request),
    )
    .await
    .expect_err("another user must not trigger paid video generation");
    assert_eq!(outsider_generate.status(), StatusCode::NOT_FOUND);

    let other_script_id = script_id + 70_000;
    let other_track_id = track_id + 70_000;
    sqlx::query(
        "INSERT INTO toonflow.scripts(id,name,content,project_id,create_time)
         VALUES($1,'归属隔离测试','',$2,$3)",
    )
    .bind(other_script_id)
    .bind(project_id)
    .bind(chrono::Utc::now().timestamp_millis())
    .execute(&pool)
    .await
    .expect("insert secondary script");
    sqlx::query(
        "INSERT INTO toonflow.video_tracks(id,project_id,script_id,state,duration)
         VALUES($1,$2,$3,'未生成',1)",
    )
    .bind(other_track_id)
    .bind(project_id)
    .bind(other_script_id)
    .execute(&pool)
    .await
    .expect("insert secondary script track");
    let mismatched_generate_request = serde_json::from_value(serde_json::json!({
        "projectId": project_id,
        "scriptId": script_id,
        "prompt": "mismatched",
        "model": "1",
        "mode": "text",
        "resolution": "720p",
        "duration": 1,
        "trackId": other_track_id,
        "uploadData": []
    }))
    .expect("mismatched video generation request");
    let mismatched_generate = toonflow_video::generate_video(
        user(),
        State(state.clone()),
        Json(mismatched_generate_request),
    )
    .await
    .expect_err("a track from another script must not trigger generation");
    assert_eq!(mismatched_generate.status(), StatusCode::NOT_FOUND);
    let mismatched_batch_request = serde_json::from_value(serde_json::json!({
        "projectId": project_id,
        "scriptId": script_id,
        "trackData": [{
            "uploadData": [],
            "trackId": other_track_id,
            "prompt": "mismatched batch",
            "duration": 1
        }],
        "model": "1",
        "mode": "text",
        "resolution": "720p"
    }))
    .expect("mismatched batch generation request");
    let mismatched_batch =
        toonflow_video::batch_videos(user(), State(state.clone()), Json(mismatched_batch_request))
            .await
            .expect_err("batch generation must reject a track from another script");
    assert_eq!(mismatched_batch.status(), StatusCode::NOT_FOUND);
    sqlx::query("DELETE FROM toonflow.scripts WHERE id=$1")
        .bind(other_script_id)
        .execute(&pool)
        .await
        .expect("remove secondary script isolation fixture");
    let _ = toonflow_video::select_video(
        user(),
        State(state.clone()),
        Json(toonflow_video::Select { track_id, video_id }),
    )
    .await
    .expect("select generated video");
    sqlx::query("UPDATE toonflow.video_tracks SET select_video_id=$2,state='生成成功' WHERE id=$1")
        .bind(track_id)
        .bind(video_id)
        .execute(&pool)
        .await
        .expect("record compatible selected video state");

    let unrelated_video_id = video_id + 1;
    sqlx::query("INSERT INTO toonflow.videos(id,file_path,state,script_id,project_id,video_track_id) VALUES($1,$2,'生成成功',$3,$4,NULL)")
        .bind(unrelated_video_id)
        .bind(&source_url)
        .bind(script_id)
        .bind(project_id)
        .execute(&pool)
        .await
        .expect("insert video outside the selected track");
    let cross_track_selection = toonflow_video::select_video(
        user(),
        State(state.clone()),
        Json(toonflow_video::Select {
            track_id,
            video_id: unrelated_video_id,
        }),
    )
    .await
    .expect_err("a video outside the track must not be selectable");
    assert_eq!(cross_track_selection.status(), StatusCode::BAD_REQUEST);
    let outsider_selection = toonflow_video::select_video(
        scoped_user("00000000-0000-0000-0000-000000000002"),
        State(state.clone()),
        Json(toonflow_video::Select { track_id, video_id }),
    )
    .await
    .expect_err("another user must not change the selected project video");
    assert_eq!(outsider_selection.status(), StatusCode::NOT_FOUND);
    sqlx::query("UPDATE toonflow.video_tracks SET video_id=$2 WHERE id=$1")
        .bind(track_id)
        .bind(unrelated_video_id)
        .execute(&pool)
        .await
        .expect("inject a legacy cross-track selection fixture");

    let export = toonflow_video_export::export(
        user(),
        State(state.clone()),
        Json(toonflow_video_export::ExportRequest {
            project_id,
            script_id,
            video_ids: vec![],
        }),
    )
    .await
    .expect("submit export");
    let task_id = export.0.data["taskId"].as_i64().expect("export task id");
    let first_result = wait_for_export(&pool, task_id).await;
    assert_eq!(first_result["videoIds"], serde_json::json!([video_id]));
    sqlx::query("UPDATE toonflow.video_tracks SET video_id=$2 WHERE id=$1")
        .bind(track_id)
        .bind(video_id)
        .execute(&pool)
        .await
        .expect("restore selected video after isolation check");
    sqlx::query("DELETE FROM toonflow.videos WHERE id=$1")
        .bind(unrelated_video_id)
        .execute(&pool)
        .await
        .expect("remove selection isolation fixture");
    let first_url = first_result["url"]
        .as_str()
        .expect("first export url")
        .to_string();
    let first_render_id = first_result["episodeRenderId"]
        .as_i64()
        .expect("first episode render id");
    assert_eq!(first_result["version"], 1);
    assert!(first_url.ends_with(".mp4"));
    assert!(first_url.contains(&format!("task-{task_id}-lease-")));
    assert!(
        toonflow_storage::asset_exists(&first_url)
            .await
            .expect("check exported asset")
    );

    let first_archive = toonflow_episode_renders::project_video_archive(
        scoped_user("00000000-0000-0000-0000-000000000001"),
        State(state.clone()),
        Path(project_id),
    )
    .await
    .expect("list first project archive")
    .0
    .data;
    assert_eq!(first_archive.project_id, project_id);
    assert_eq!(first_archive.episodes.len(), 1);
    let first_episode = &first_archive.episodes[0];
    assert_eq!(first_episode.script_id, script_id);
    assert_eq!(first_episode.script_name, "第1集");
    assert_eq!(first_episode.episode_no, Some(1));
    assert_eq!(first_episode.renders.len(), 1);
    let first_render = &first_episode.renders[0];
    assert_eq!(first_render.id, first_render_id);
    assert_eq!(first_render.version, 1);
    assert!(first_render.is_current);
    assert_eq!(first_render.state, "ready");
    assert_eq!(first_render.source_video_ids, vec![video_id]);
    assert_eq!(first_render.created_by, user().user_id);
    assert_eq!(first_render.export_task_id, Some(task_id));
    assert_eq!(first_render.url, first_url);
    assert_eq!(first_render.file_path, first_url);
    assert!(!first_render.object_path.starts_with('/'));
    assert_eq!(first_render.metadata["sourceCount"], 1);
    let media_key = first_url
        .strip_prefix("/toonflow/assets/files/")
        .expect("exported media key")
        .to_string();
    let mut range_headers = HeaderMap::new();
    range_headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1023"));
    let media_response = toonflow_storage::serve_image(
        scoped_user("00000000-0000-0000-0000-000000000001"),
        State(state.clone()),
        Path(media_key.clone()),
        range_headers,
    )
    .await
    .expect("owner streams archived media");
    assert_eq!(media_response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        media_response.headers()[header::CACHE_CONTROL],
        "private, max-age=300"
    );
    assert_eq!(
        media_response.headers()[header::REFERRER_POLICY],
        "no-referrer"
    );
    let media_chunk = to_bytes(media_response.into_body(), 2048)
        .await
        .expect("read bounded media range");
    assert!(!media_chunk.is_empty());
    assert!(media_chunk.len() <= 1024);
    let outsider_media_error = toonflow_storage::serve_image(
        scoped_user("00000000-0000-0000-0000-000000000002"),
        State(state.clone()),
        Path(media_key),
        HeaderMap::new(),
    )
    .await
    .expect_err("another project owner must not stream archived media");
    assert_eq!(outsider_media_error.status(), StatusCode::NOT_FOUND);
    let cleanup_guard_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.storage_cleanup_tasks(
           object_path,resource_type,resource_id,error_reason,state,create_time,update_time
         ) VALUES(
           $1,'e2e_reference_guard',$2,'verify live-reference protection','pending',
           (extract(epoch FROM clock_timestamp())*1000)::bigint,
           (extract(epoch FROM clock_timestamp())*1000)::bigint
         ) RETURNING id",
    )
    .bind(&first_url)
    .bind(first_render_id)
    .fetch_one(&pool)
    .await
    .expect("queue a cleanup request for a referenced render");
    let mut cleanup_guard = None;
    for _ in 0..150 {
        cleanup_guard = sqlx::query_as::<_, (String, String)>(
            "SELECT state,error_reason FROM toonflow.storage_cleanup_tasks WHERE id=$1",
        )
        .bind(cleanup_guard_id)
        .fetch_optional(&pool)
        .await
        .expect("poll reference-aware cleanup");
        if cleanup_guard
            .as_ref()
            .is_some_and(|(state, _)| state == "completed")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let cleanup_guard = cleanup_guard.expect("cleanup guard row");
    assert_eq!(cleanup_guard.0, "completed");
    assert!(cleanup_guard.1.contains("仍被业务记录引用"));
    assert!(
        toonflow_storage::asset_exists(&first_url)
            .await
            .expect("referenced render survives cleanup")
    );
    let outsider_error = toonflow_episode_renders::project_video_archive(
        scoped_user("00000000-0000-0000-0000-000000000002"),
        State(state.clone()),
        Path(project_id),
    )
    .await
    .expect_err("another user must not read the project archive");
    assert_eq!(outsider_error.status(), axum::http::StatusCode::NOT_FOUND);

    let second_export = toonflow_video_export::export(
        user(),
        State(state.clone()),
        Json(toonflow_video_export::ExportRequest {
            project_id,
            script_id,
            video_ids: vec![video_id],
        }),
    )
    .await
    .expect("submit second export");
    let second_task_id = second_export.0.data["taskId"]
        .as_i64()
        .expect("second export task id");
    let second_result = wait_for_export(&pool, second_task_id).await;
    let second_url = second_result["url"]
        .as_str()
        .expect("second export url")
        .to_string();
    let second_render_id = second_result["episodeRenderId"]
        .as_i64()
        .expect("second episode render id");
    assert_eq!(second_result["version"], 2);
    assert_ne!(first_url, second_url);
    assert!(second_url.contains(&format!("task-{second_task_id}-lease-")));

    let failed_render_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.episode_renders(
             project_id,script_id,version,object_path,file_path,status,
             source_video_ids,metadata,is_current,created_by
         )
         VALUES($1,$2,3,'exports/failed.mp4','/not-an-asset/failed.mp4','failed',
                ARRAY[$3]::bigint[],'{}'::jsonb,false,$4)
         RETURNING id",
    )
    .bind(project_id)
    .bind(script_id)
    .bind(video_id)
    .bind(uuid::Uuid::parse_str(&user().user_id).expect("test user uuid"))
    .fetch_one(&pool)
    .await
    .expect("insert a non-ready render for visibility checks");

    let filtered_archive = toonflow_episode_renders::project_video_archive(
        scoped_user("00000000-0000-0000-0000-000000000001"),
        State(state.clone()),
        Path(project_id),
    )
    .await
    .expect("list archive without non-ready renders")
    .0
    .data;
    assert_eq!(filtered_archive.episodes[0].renders.len(), 2);
    assert!(
        filtered_archive.episodes[0]
            .renders
            .iter()
            .all(|render| render.state == "ready" && render.id != failed_render_id)
    );

    let versions = toonflow_episode_renders::list_episode_renders(
        scoped_user("00000000-0000-0000-0000-000000000001"),
        State(state.clone()),
        Path((project_id, script_id)),
    )
    .await
    .expect("list episode render versions")
    .0
    .data;
    assert_eq!(versions.project_id, project_id);
    assert_eq!(versions.script_id, script_id);
    assert_eq!(versions.renders.len(), 2);
    assert_eq!(versions.renders[0].id, second_render_id);
    assert_eq!(versions.renders[0].version, 2);
    assert!(versions.renders[0].is_current);
    assert_eq!(versions.renders[1].id, first_render_id);
    assert!(!versions.renders[1].is_current);
    let failed_current = toonflow_episode_renders::select_current_render(
        scoped_user("00000000-0000-0000-0000-000000000001"),
        State(state.clone()),
        Path(failed_render_id),
    )
    .await
    .expect_err("a non-ready render must not become current");
    assert_eq!(failed_current.status(), axum::http::StatusCode::BAD_REQUEST);
    let mismatched_script = toonflow_episode_renders::list_episode_renders(
        scoped_user("00000000-0000-0000-0000-000000000001"),
        State(state.clone()),
        Path((project_id, script_id + 1)),
    )
    .await
    .expect_err("a mismatched project and script must be hidden");
    assert_eq!(
        mismatched_script.status(),
        axum::http::StatusCode::NOT_FOUND
    );

    let selected = toonflow_episode_renders::select_current_render(
        scoped_user("00000000-0000-0000-0000-000000000001"),
        State(state.clone()),
        Path(first_render_id),
    )
    .await
    .expect("select first render as current")
    .0
    .data;
    assert_eq!(selected.id, first_render_id);
    assert!(selected.is_current);
    let current_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM toonflow.episode_renders
         WHERE project_id=$1 AND script_id=$2 AND is_current",
    )
    .bind(project_id)
    .bind(script_id)
    .fetch_all(&pool)
    .await
    .expect("inspect unique current render");
    assert_eq!(current_ids, vec![first_render_id]);

    let _ = toonflow_project_crud::delete_project(
        user(),
        State(state),
        Json(toonflow::IdRequest { id: project_id }),
    )
    .await
    .expect("delete project and archived objects");
    for _ in 0..150 {
        let mut any_exists = false;
        for path in [&first_url, &second_url, &source_url] {
            any_exists |= toonflow_storage::asset_exists(path)
                .await
                .expect("poll deleted project object");
        }
        if !any_exists {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    for path in [&first_url, &second_url, &source_url] {
        assert!(
            !toonflow_storage::asset_exists(path)
                .await
                .expect("inspect deleted project object"),
            "project deletion left an orphan object: {path}"
        );
    }
    let remaining_renders: i64 =
        sqlx::query_scalar("SELECT count(*) FROM toonflow.episode_renders WHERE project_id=$1")
            .bind(project_id)
            .fetch_one(&pool)
            .await
            .expect("inspect deleted project renders");
    assert_eq!(remaining_renders, 0);
}
