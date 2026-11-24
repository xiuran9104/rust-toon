use axum::{
    Json,
    extract::{Query, State},
};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde_json::{Value, json};

use crate::toonflow_workflow::{
    LatestNodeRunQuery, NodeRunIdRequest, StartWorkflowNodeResponse, StoryboardImageNodeInput,
    WorkflowNodeRunResponse, WorkflowRunIdRequest, active_node_runs, active_workflow_runs,
    ensure_workflow_run_access, launch_node, recover_stale_node_runs,
    rollback_partial_storyboard_panel,
};
use crate::{
    ToonState,
    shared::require,
    toonflow_episode_renders::{ensure_project_access, ensure_script_in_project},
    toonflow_video,
};

pub async fn cancel_run(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<WorkflowRunIdRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let run = sqlx::query_as::<_, (String, i64, i64)>(
        "SELECT state,project_id,script_id FROM toonflow.workflow_runs WHERE id=$1",
    )
    .bind(request.id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load workflow run"))?
    .ok_or_else(|| AppError::not_found("workflow run not found"))?;
    ensure_workflow_run_access(&state.pool, &user, request.id).await?;
    if matches!(run.0.as_str(), "success" | "failed" | "cancelled") {
        return Err(AppError::bad_request("workflow run has already finished"));
    }
    // 先置取消标志：节点任务在下一个检查点即停止（对其他副本同样生效），
    // 本进程的 abort 仍然即时。
    let _ = sqlx::query(
        "UPDATE toonflow.workflow_runs SET cancel_requested=true WHERE id=$1 AND state='running'",
    )
    .bind(request.id)
    .execute(&state.pool)
    .await;
    if let Some(handle) = active_workflow_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&request.id)
    {
        handle.abort();
    }
    let running_nodes = sqlx::query_as::<_, (i64, String, Value)>(
        "SELECT id,node_type,input FROM toonflow.workflow_node_runs
         WHERE workflow_run_id=$1 AND state='running'",
    )
    .bind(request.id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load active workflow nodes"))?;
    for (node_run_id, node_type, input) in &running_nodes {
        if let Some(handle) = active_node_runs()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(node_run_id)
        {
            handle.abort();
        }
        if node_type == "storyboard.plan" {
            rollback_partial_storyboard_panel(&state.pool, run.1, run.2, input)
                .await
                .map_err(|_| AppError::internal("failed to roll back partial storyboard panel"))?;
        } else if node_type == "storyboard.image" {
            if let Ok(input) = serde_json::from_value::<StoryboardImageNodeInput>(input.clone()) {
                let _ = sqlx::query(
                    "UPDATE toonflow.storyboards SET state='已取消',reason='用户取消生成'
                     WHERE id=ANY($1) AND project_id=$2 AND script_id=$3 AND state='生成中'",
                )
                .bind(input.storyboard_ids)
                .bind(run.1)
                .bind(run.2)
                .execute(&state.pool)
                .await;
            }
        } else if node_type == "video.generate"
            && let Ok(input) =
                serde_json::from_value::<toonflow_video::WorkflowVideoInput>(input.clone())
        {
            let _ = sqlx::query(
                "UPDATE toonflow.videos SET state='已取消',error_reason='用户取消生成'
                 WHERE id=ANY($1) AND project_id=$2 AND script_id=$3 AND state='生成中'",
            )
            .bind(input.video_ids)
            .bind(run.1)
            .bind(run.2)
            .execute(&state.pool)
            .await;
        }
    }
    let time = chrono::Utc::now().timestamp_millis();
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin workflow cancellation"))?;
    sqlx::query(
        "UPDATE toonflow.workflow_node_runs
         SET state='cancelled',error_reason='用户取消工作流',finish_time=$2
         WHERE workflow_run_id=$1 AND state IN('pending','blocked','running')",
    )
    .bind(request.id)
    .bind(time)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to cancel workflow nodes"))?;
    sqlx::query(
        "UPDATE toonflow.workflow_runs
         SET state='cancelled',error_reason='用户取消工作流',finish_time=$2 WHERE id=$1",
    )
    .bind(request.id)
    .bind(time)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to cancel workflow run"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit workflow cancellation"))?;
    Ok(Json(ApiResponse::new(
        json!({"id":request.id,"state":"cancelled"}),
    )))
}

pub async fn latest_node_run(
    user: CurrentUser,
    State(state): State<ToonState>,
    Query(query): Query<LatestNodeRunQuery>,
) -> Result<Json<ApiResponse<Option<WorkflowNodeRunResponse>>>, AppError> {
    require(&user, "toon:scene:read")?;
    ensure_project_access(&state.pool, &user, query.project_id).await?;
    ensure_script_in_project(&state.pool, query.project_id, query.script_id).await?;
    recover_stale_node_runs(&state).await;
    let row = sqlx::query_as::<_, WorkflowNodeRunResponse>(
        "SELECT nr.id,nr.workflow_run_id,nr.agent_run_id,nr.node_id,nr.node_type,nr.attempt,nr.state,
                nr.input,nr.output,nr.error_reason,nr.progress_current,nr.progress_total,
                nr.retry_of_id,nr.start_time,nr.finish_time,nr.create_time
         FROM toonflow.workflow_node_runs nr
         JOIN toonflow.workflow_runs r ON r.id=nr.workflow_run_id
         WHERE r.project_id=$1 AND r.script_id=$2 AND nr.node_id=$3
         ORDER BY nr.create_time DESC,nr.id DESC LIMIT 1",
    )
    .bind(query.project_id)
    .bind(query.script_id)
    .bind(&query.node_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|error| {
        tracing::error!(?error, project_id = query.project_id, script_id = query.script_id, node_id = %query.node_id, "latest workflow node run query failed");
        AppError::internal("failed to load latest workflow node run")
    })?;
    Ok(Json(ApiResponse::new(row)))
}

pub async fn cancel_node(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<NodeRunIdRequest>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "toon:scene:update")?;
    let row = sqlx::query_as::<_, (i64, String, Value, i64, i64)>(
        "SELECT nr.workflow_run_id,nr.node_type,nr.input,r.project_id,r.script_id
         FROM toonflow.workflow_node_runs nr
         JOIN toonflow.workflow_runs r ON r.id=nr.workflow_run_id
         WHERE nr.id=$1 AND nr.state='running'",
    )
    .bind(request.id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load running workflow node"))?
    .ok_or_else(|| AppError::bad_request("workflow node run has already finished"))?;
    ensure_workflow_run_access(&state.pool, &user, row.0).await?;
    // 先置取消标志（节点与所属运行）：节点任务在下一个检查点即停止。
    let _ = sqlx::query(
        "UPDATE toonflow.workflow_node_runs nr
         SET cancel_requested=true
         FROM toonflow.workflow_runs r
         WHERE nr.id=$1 AND nr.state='running'",
    )
    .bind(request.id)
    .execute(&state.pool)
    .await;
    let _ = sqlx::query(
        "UPDATE toonflow.workflow_runs SET cancel_requested=true WHERE id=$1 AND state='running'",
    )
    .bind(row.0)
    .execute(&state.pool)
    .await;
    if let Some(handle) = active_node_runs()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .remove(&request.id)
    {
        handle.abort();
    }
    if row.1 == "storyboard.plan" {
        rollback_partial_storyboard_panel(&state.pool, row.3, row.4, &row.2)
            .await
            .map_err(|_| AppError::internal("failed to roll back partial storyboard panel"))?;
    }
    let time = chrono::Utc::now().timestamp_millis();
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin workflow cancellation"))?;
    if row.1 == "storyboard.image" {
        let input: StoryboardImageNodeInput = serde_json::from_value(row.2.clone())
            .map_err(|_| AppError::internal("stored storyboard node input is invalid"))?;
        sqlx::query(
            "UPDATE toonflow.storyboards SET state='已取消',reason='用户取消生成'
             WHERE id=ANY($1) AND project_id=$2 AND script_id=$3 AND state='生成中'",
        )
        .bind(&input.storyboard_ids)
        .bind(row.3)
        .bind(row.4)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AppError::internal("failed to cancel storyboard generation"))?;
    } else if row.1 == "video.generate" {
        let input: toonflow_video::WorkflowVideoInput = serde_json::from_value(row.2.clone())
            .map_err(|_| AppError::internal("stored video node input is invalid"))?;
        sqlx::query(
            "UPDATE toonflow.videos SET state='已取消',error_reason='用户取消生成'
             WHERE id=ANY($1) AND project_id=$2 AND script_id=$3 AND state='生成中'",
        )
        .bind(&input.video_ids)
        .bind(row.3)
        .bind(row.4)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AppError::internal("failed to cancel video generation"))?;
    }
    sqlx::query(
        "UPDATE toonflow.workflow_node_runs
         SET state='cancelled',error_reason='用户取消生成',finish_time=$2
         WHERE id=$1 AND state='running'",
    )
    .bind(request.id)
    .bind(time)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to cancel workflow node run"))?;
    sqlx::query(
        "UPDATE toonflow.workflow_runs
         SET state='cancelled',error_reason='用户取消生成',finish_time=$2 WHERE id=$1",
    )
    .bind(row.0)
    .bind(time)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to cancel workflow run"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit workflow cancellation"))?;
    Ok(Json(ApiResponse::new(
        json!({"id": request.id, "state": "cancelled"}),
    )))
}

pub async fn retry_node(
    user: CurrentUser,
    State(state): State<ToonState>,
    Json(request): Json<NodeRunIdRequest>,
) -> Result<Json<ApiResponse<StartWorkflowNodeResponse>>, AppError> {
    require(&user, "toon:scene:update")?;
    recover_stale_node_runs(&state).await;
    let source = sqlx::query_as::<_, (i64, String, String, i32, String, Value)>(
        "SELECT workflow_run_id,node_id,node_type,attempt,state,input
         FROM toonflow.workflow_node_runs WHERE id=$1",
    )
    .bind(request.id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load workflow retry source"))?
    .ok_or_else(|| AppError::not_found("workflow node run not found"))?;
    let (project_id, script_id) = ensure_workflow_run_access(&state.pool, &user, source.0).await?;
    if !matches!(source.4.as_str(), "failed" | "cancelled") {
        return Err(AppError::bad_request(
            "only failed or cancelled workflow nodes can be retried",
        ));
    }
    let mut input = source.5;
    if source.2 == "storyboard.image" {
        let mut storyboard_input: StoryboardImageNodeInput = serde_json::from_value(input)
            .map_err(|_| AppError::internal("stored storyboard node input is invalid"))?;
        storyboard_input.storyboard_ids = sqlx::query_scalar(
            "SELECT id FROM toonflow.storyboards
             WHERE id=ANY($1) AND project_id=$2 AND script_id=$3
               AND state IN('生成失败','已取消') ORDER BY id",
        )
        .bind(&storyboard_input.storyboard_ids)
        .bind(project_id)
        .bind(script_id)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to load retryable storyboards"))?;
        if storyboard_input.storyboard_ids.is_empty() {
            return Err(AppError::bad_request(
                "no failed or cancelled storyboards to retry",
            ));
        }
        input = serde_json::to_value(storyboard_input)
            .map_err(|_| AppError::internal("failed to serialize storyboard retry input"))?;
    } else if source.2 == "video.generate" {
        let mut video_input: toonflow_video::WorkflowVideoInput = serde_json::from_value(input)
            .map_err(|_| AppError::internal("stored video node input is invalid"))?;
        video_input.video_ids.clear();
        input = serde_json::to_value(video_input)
            .map_err(|_| AppError::internal("failed to serialize video retry input"))?;
    }
    let node_run_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.workflow_node_runs
         (workflow_run_id,node_id,node_type,attempt,state,input,retry_of_id,create_time)
         VALUES($1,$2,$3,$4,'pending',$5,$6,$7) RETURNING id",
    )
    .bind(source.0)
    .bind(source.1)
    .bind(source.2)
    .bind(source.3 + 1)
    .bind(&input)
    .bind(request.id)
    .bind(chrono::Utc::now().timestamp_millis())
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to create workflow node retry"))?;
    let response = launch_node(&state, node_run_id, input).await?;
    Ok(Json(ApiResponse::new(response)))
}
