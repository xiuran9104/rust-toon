use std::{collections::HashMap, env, io::Write, time::Instant};

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        DefaultBodyLimit, Multipart, Path, Query, Request, State, multipart::MultipartError,
    },
    http::{StatusCode, header},
    middleware::{Next, from_fn, from_fn_with_state},
    response::Response,
    routing::{delete, get, post, put},
};
use chrono::Utc;
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_database::PgPool;
use rust_toon_framework_jobs::{
    INFRA_SCHEDULED_JOB_KIND, ScheduledJobPayload, next_occurrence, next_occurrences, validate_cron,
};
use rust_toon_framework_security::{
    CurrentUser, Permission, TokenService, authenticate, seal_secret,
};
use rust_toon_framework_telemetry::{current_trace_context, current_trace_id};
use rust_toon_framework_web::AppError;
use rust_toon_infra_api::InfraCapability;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::QueryBuilder;
use uuid::Uuid;

mod excel;
mod monitor;
mod object_storage;

const DEFAULT_UPLOAD_MAX_BYTES: usize = 20 * 1024 * 1024;

/// Read an existing application upload without making an HTTP request back to
/// the Gateway. Only the fixed `infra/` object namespace can be addressed.
pub async fn read_uploaded_file(pool: &PgPool, path: &str, max_bytes: usize) -> Result<Vec<u8>, AppError> {
    let path = path.strip_prefix("/api").unwrap_or(path);
    let relative = path.strip_prefix("/upload/")
        .filter(|value| !value.is_empty() && !value.contains(['?', '#', '%', '\\'])
            && value.split('/').all(|part| !matches!(part, "" | "." | "..")))
        .ok_or_else(|| AppError::bad_request("无效的上传文件路径"))?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM infra_file WHERE path=$1 AND deleted=0)")
        .bind(path).fetch_one(pool).await
        .map_err(|_| AppError::internal("读取上传文件记录失败"))?;
    if !exists { return Err(AppError::not_found("上传文件不存在")); }
    object_storage::get_bounded(&format!("infra/{relative}"), max_bytes).await
        .map_err(AppError::bad_request)
}
const MAX_CONFIGURABLE_UPLOAD_BYTES: usize = 100 * 1024 * 1024;
// `DefaultBodyLimit` applies to the complete multipart envelope, while the
// configured limit describes the file itself. Keep a small, bounded allowance
// for the boundary and content-disposition headers and enforce the exact file
// limit again after extraction.
const MULTIPART_OVERHEAD_BYTES: usize = 64 * 1024;
const INFRA_JOB_STATUS_NORMAL: i16 = 1;

#[derive(Clone)]
pub struct InfraState {
    pool: PgPool,
    tokens: TokenService,
    upload_max_bytes: usize,
    started_at: Instant,
}

impl InfraState {
    pub fn new(pool: PgPool, tokens: TokenService) -> Self {
        Self {
            pool,
            tokens,
            upload_max_bytes: configured_upload_max_bytes(),
            started_at: Instant::now(),
        }
    }
}

#[derive(Debug, Serialize)]
struct Page<T> {
    list: Vec<T>,
    total: i64,
}

#[derive(Debug, Deserialize)]
pub struct QueryParams {
    #[serde(default, rename = "pageNo")]
    page_no: Option<i64>,
    #[serde(default, rename = "pageSize")]
    page_size: Option<i64>,
    #[serde(default, rename = "userId")]
    user_id: Option<i64>,
    #[serde(default, rename = "userType")]
    user_type: Option<i16>,
    #[serde(default, rename = "applicationName")]
    application_name: Option<String>,
    #[serde(default)]
    duration: Option<i32>,
    #[serde(default, rename = "resultCode")]
    result_code: Option<i32>,
}

pub fn routes(state: InfraState) -> Router {
    let protected = Router::new()
        .route("/infra/config/page", get(config_page))
        .route("/infra/config/get", get(config_get))
        .route("/infra/config/get-value-by-key", get(config_value_by_key))
        .route("/infra/config/create", post(config_create))
        .route("/infra/config/update", put(config_update))
        .route("/infra/config/delete", delete(config_delete))
        .route("/infra/config/delete-list", delete(config_delete_list))
        .route("/infra/config/export-excel", get(excel::config_export))
        .route("/infra/data-source-config/list", get(data_source_list))
        .route("/infra/data-source-config/get", get(data_source_get))
        .route("/infra/data-source-config/create", post(data_source_create))
        .route("/infra/data-source-config/update", put(data_source_update))
        .route(
            "/infra/data-source-config/delete",
            delete(data_source_delete),
        )
        .route(
            "/infra/data-source-config/delete-list",
            delete(data_source_delete_list),
        )
        .route("/infra/file-config/page", get(file_config_page))
        .route("/infra/file-config/get", get(file_config_get))
        .route("/infra/file-config/create", post(file_config_create))
        .route("/infra/file-config/update", put(file_config_update))
        .route("/infra/file-config/update-master", put(file_config_master))
        .route("/infra/file-config/delete", delete(file_config_delete))
        .route(
            "/infra/file-config/delete-list",
            delete(file_config_delete_list),
        )
        .route("/infra/file-config/test", get(ok_bool))
        .route("/infra/file/page", get(file_page))
        .route("/infra/file/create", post(file_create))
        .route(
            "/infra/file/upload",
            post(file_upload).layer(DefaultBodyLimit::max(
                state
                    .upload_max_bytes
                    .saturating_add(MULTIPART_OVERHEAD_BYTES),
            )),
        )
        .route("/infra/file/presigned-url", get(file_presigned_url))
        .route("/infra/file/delete", delete(file_delete))
        .route("/infra/file/delete-list", delete(file_delete_list))
        .route("/infra/job/page", get(job_page))
        .route("/infra/job/get", get(job_get))
        .route("/infra/job/create", post(job_create))
        .route("/infra/job/update", put(job_update))
        .route("/infra/job/update-status", put(job_update_status))
        .route("/infra/job/trigger", put(job_trigger))
        .route("/infra/job/get_next_times", get(job_next_times))
        .route("/infra/job/sync", post(job_sync))
        .route("/infra/job/delete", delete(job_delete))
        .route("/infra/job/delete-list", delete(job_delete_list))
        .route("/infra/job/export-excel", get(excel::job_export))
        .route("/infra/job-log/page", get(job_log_page))
        .route("/infra/job-log/export-excel", get(excel::job_log_export))
        .route("/infra/api-access-log/page", get(api_access_log_page))
        .route(
            "/infra/api-access-log/export-excel",
            get(excel::api_access_log_export),
        )
        .route("/infra/api-error-log/page", get(api_error_log_page))
        .route(
            "/infra/api-error-log/update-status",
            put(api_error_log_update_status),
        )
        .route(
            "/infra/api-error-log/export-excel",
            get(excel::api_error_log_export),
        )
        .route("/infra/redis/get-monitor-info", get(redis_monitor_info))
        .route("/infra/monitor/postgresql", get(monitor::postgresql))
        .route("/infra/monitor/rust", get(monitor::rust_service))
        .route("/infra/monitor/traces", get(monitor::traces))
        .route("/infra/codegen/table/list", get(codegen_table_list))
        .route("/infra/codegen/table/page", get(codegen_table_page))
        .route("/infra/codegen/detail", get(codegen_detail))
        .route("/infra/codegen/update", put(codegen_update))
        .route("/infra/codegen/sync-from-db", put(codegen_sync_from_db))
        .route("/infra/codegen/preview", get(codegen_preview))
        .route("/infra/codegen/download", get(codegen_download))
        .route("/infra/codegen/db/table/list", get(codegen_db_table_list))
        .route("/infra/codegen/create-list", post(codegen_create_list))
        .route("/infra/codegen/delete", delete(codegen_delete))
        .route("/infra/codegen/delete-list", delete(codegen_delete_list))
        .route("/infra/demo01-contact/page", get(demo01_contact_page))
        .route("/infra/demo01-contact/get", get(demo01_contact_get))
        .route("/infra/demo01-contact/create", post(demo01_contact_create))
        .route("/infra/demo01-contact/update", put(demo01_contact_update))
        .route(
            "/infra/demo01-contact/delete",
            delete(demo01_contact_delete),
        )
        .route(
            "/infra/demo01-contact/delete-list",
            delete(demo01_contact_delete_list),
        )
        .route(
            "/infra/demo01-contact/export-excel",
            get(excel::demo01_contact_export),
        )
        .route("/infra/demo02-category/list", get(demo02_category_list))
        .route("/infra/demo02-category/get", get(demo02_category_get))
        .route(
            "/infra/demo02-category/create",
            post(demo02_category_create),
        )
        .route("/infra/demo02-category/update", put(demo02_category_update))
        .route(
            "/infra/demo02-category/delete",
            delete(demo02_category_delete),
        )
        .route(
            "/infra/demo02-category/export-excel",
            get(excel::demo02_category_export),
        )
        .route(
            "/infra/demo03-student-normal/page",
            get(demo03_student_page),
        )
        .route("/infra/demo03-student-normal/get", get(demo03_student_get))
        .route(
            "/infra/demo03-student-normal/create",
            post(demo03_student_create),
        )
        .route(
            "/infra/demo03-student-normal/update",
            put(demo03_student_update),
        )
        .route(
            "/infra/demo03-student-normal/delete",
            delete(demo03_student_delete),
        )
        .route(
            "/infra/demo03-student-normal/delete-list",
            delete(demo03_student_delete_list),
        )
        .route(
            "/infra/demo03-student-normal/export-excel",
            get(excel::demo03_student_export),
        )
        .route(
            "/infra/demo03-student-normal/demo03-course/list-by-student-id",
            get(demo03_course_list_by_student_id),
        )
        .route(
            "/infra/demo03-student-normal/demo03-grade/get-by-student-id",
            get(demo03_grade_get_by_student_id),
        )
        .route("/infra/demo03-student-inner/page", get(demo03_student_page))
        .route("/infra/demo03-student-inner/get", get(demo03_student_get))
        .route(
            "/infra/demo03-student-inner/create",
            post(demo03_student_create),
        )
        .route(
            "/infra/demo03-student-inner/update",
            put(demo03_student_update),
        )
        .route(
            "/infra/demo03-student-inner/delete",
            delete(demo03_student_delete),
        )
        .route(
            "/infra/demo03-student-inner/delete-list",
            delete(demo03_student_delete_list),
        )
        .route(
            "/infra/demo03-student-inner/export-excel",
            get(excel::demo03_student_export),
        )
        .route(
            "/infra/demo03-student-inner/demo03-course/list-by-student-id",
            get(demo03_course_list_by_student_id),
        )
        .route(
            "/infra/demo03-student-inner/demo03-grade/get-by-student-id",
            get(demo03_grade_get_by_student_id),
        )
        .route("/infra/demo03-student-erp/page", get(demo03_student_page))
        .route("/infra/demo03-student-erp/get", get(demo03_student_get))
        .route(
            "/infra/demo03-student-erp/create",
            post(demo03_student_create),
        )
        .route(
            "/infra/demo03-student-erp/update",
            put(demo03_student_update),
        )
        .route(
            "/infra/demo03-student-erp/delete",
            delete(demo03_student_delete),
        )
        .route(
            "/infra/demo03-student-erp/delete-list",
            delete(demo03_student_delete_list),
        )
        .route(
            "/infra/demo03-student-erp/export-excel",
            get(excel::demo03_student_export),
        )
        .route(
            "/infra/demo03-student-erp/demo03-course/page",
            get(demo03_course_page),
        )
        .route(
            "/infra/demo03-student-erp/demo03-course/get",
            get(demo03_course_get),
        )
        .route(
            "/infra/demo03-student-erp/demo03-course/create",
            post(demo03_course_create),
        )
        .route(
            "/infra/demo03-student-erp/demo03-course/update",
            put(demo03_course_update),
        )
        .route(
            "/infra/demo03-student-erp/demo03-course/delete",
            delete(demo03_course_delete),
        )
        .route(
            "/infra/demo03-student-erp/demo03-course/delete-list",
            delete(demo03_course_delete_list),
        )
        .route(
            "/infra/demo03-student-erp/demo03-grade/page",
            get(demo03_grade_page),
        )
        .route(
            "/infra/demo03-student-erp/demo03-grade/get",
            get(demo03_grade_get),
        )
        .route(
            "/infra/demo03-student-erp/demo03-grade/create",
            post(demo03_grade_create),
        )
        .route(
            "/infra/demo03-student-erp/demo03-grade/update",
            put(demo03_grade_update),
        )
        .route(
            "/infra/demo03-student-erp/demo03-grade/delete",
            delete(demo03_grade_delete),
        )
        .route(
            "/infra/demo03-student-erp/demo03-grade/delete-list",
            delete(demo03_grade_delete_list),
        )
        .route_layer(from_fn(authorize_infra))
        .route_layer(from_fn_with_state(state.tokens.clone(), authenticate));

    Router::new()
        .route("/infra/capabilities", get(capabilities))
        .route("/upload/{*path}", get(file_download))
        .merge(protected)
        .with_state(state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InfraPolicy {
    Authenticated,
    SuperAdmin,
    Permission(&'static str),
}

async fn authorize_infra(request: Request, next: Next) -> Result<Response, AppError> {
    let user = request
        .extensions()
        .get::<CurrentUser>()
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    if user.role_codes.iter().any(|role| role == "super_admin") {
        return Ok(next.run(request).await);
    }

    match infra_policy(request.uri().path()) {
        Some(InfraPolicy::Authenticated) => Ok(next.run(request).await),
        Some(InfraPolicy::SuperAdmin) => Err(AppError::forbidden("permission denied")),
        Some(InfraPolicy::Permission(code)) => {
            let permission =
                Permission::new(code).map_err(|_| AppError::internal("invalid policy"))?;
            if user.can(&permission) {
                Ok(next.run(request).await)
            } else {
                Err(AppError::forbidden("permission denied"))
            }
        }
        None => Err(AppError::forbidden(
            "infrastructure route has no access policy",
        )),
    }
}

fn infra_policy(path: &str) -> Option<InfraPolicy> {
    use InfraPolicy::{Authenticated, Permission as Allow, SuperAdmin};

    let policy = match path {
        "/infra/config/page" | "/infra/config/get" | "/infra/config/get-value-by-key" => {
            Allow("infra:config:query")
        }
        "/infra/config/create" => Allow("infra:config:create"),
        "/infra/config/update" => Allow("infra:config:update"),
        "/infra/config/delete" | "/infra/config/delete-list" => Allow("infra:config:delete"),
        "/infra/config/export-excel" => Allow("infra:config:export"),

        "/infra/data-source-config/list" | "/infra/data-source-config/get" => {
            Allow("infra:data-source-config:query")
        }
        "/infra/data-source-config/create" => Allow("infra:data-source-config:create"),
        "/infra/data-source-config/update" => Allow("infra:data-source-config:update"),
        "/infra/data-source-config/delete" | "/infra/data-source-config/delete-list" => {
            Allow("infra:data-source-config:delete")
        }

        "/infra/file-config/page" | "/infra/file-config/get" | "/infra/file-config/test" => {
            Allow("infra:file-config:query")
        }
        "/infra/file-config/create" => Allow("infra:file-config:create"),
        "/infra/file-config/update" | "/infra/file-config/update-master" => {
            Allow("infra:file-config:update")
        }
        "/infra/file-config/delete" | "/infra/file-config/delete-list" => {
            Allow("infra:file-config:delete")
        }

        "/infra/file/page" => Allow("infra:file:query"),
        // Shared profile, chat, editor, and knowledge uploads intentionally only
        // require a valid account. `presigned-url` and `create` are the two
        // companion calls used by the optional browser-to-S3 upload mode.
        // File administration still requires the query/delete permissions.
        "/infra/file/upload" | "/infra/file/presigned-url" | "/infra/file/create" => Authenticated,
        "/infra/file/delete" | "/infra/file/delete-list" => Allow("infra:file:delete"),

        "/infra/job/page"
        | "/infra/job/get"
        | "/infra/job/get_next_times"
        | "/infra/job-log/page" => Allow("infra:job:query"),
        "/infra/job/create" => Allow("infra:job:create"),
        "/infra/job/update" | "/infra/job/update-status" | "/infra/job/sync" => {
            Allow("infra:job:update")
        }
        "/infra/job/trigger" => Allow("infra:job:trigger"),
        "/infra/job/delete" | "/infra/job/delete-list" => Allow("infra:job:delete"),
        "/infra/job/export-excel" | "/infra/job-log/export-excel" => Allow("infra:job:export"),

        "/infra/api-access-log/page" => Allow("infra:api-access-log:query"),
        "/infra/api-access-log/export-excel" => Allow("infra:api-access-log:export"),
        "/infra/api-error-log/page" => Allow("infra:api-error-log:query"),
        "/infra/api-error-log/update-status" => Allow("infra:api-error-log:update-status"),
        "/infra/api-error-log/export-excel" => Allow("infra:api-error-log:export"),
        "/infra/redis/get-monitor-info" => Allow("infra:redis:get-monitor-info"),
        // These views expose database activity and cross-user request traces.
        // Their legacy menu entries have no permission codes, so fail closed
        // to the existing super-admin role instead of exposing them to every
        // authenticated account.
        "/infra/monitor/postgresql" | "/infra/monitor/rust" | "/infra/monitor/traces" => SuperAdmin,

        "/infra/codegen/table/list"
        | "/infra/codegen/table/page"
        | "/infra/codegen/detail"
        | "/infra/codegen/db/table/list" => Allow("infra:codegen:query"),
        "/infra/codegen/update" | "/infra/codegen/sync-from-db" => Allow("infra:codegen:update"),
        "/infra/codegen/preview" => Allow("infra:codegen:preview"),
        "/infra/codegen/download" => Allow("infra:codegen:download"),
        "/infra/codegen/create-list" => Allow("infra:codegen:create"),
        "/infra/codegen/delete" | "/infra/codegen/delete-list" => Allow("infra:codegen:delete"),

        path if path.starts_with("/infra/demo01-contact/") => {
            demo_policy(path, "/infra/demo01-contact/", "infra:demo01-contact")?
        }
        path if path.starts_with("/infra/demo02-category/") => {
            demo_policy(path, "/infra/demo02-category/", "infra:demo02-category")?
        }
        path if path.starts_with("/infra/demo03-student-") => {
            let action = path.rsplit('/').next()?;
            demo_action_policy(action, "infra:demo03-student")?
        }
        _ => return None,
    };
    Some(policy)
}

fn demo_policy(path: &str, prefix: &str, permission_prefix: &'static str) -> Option<InfraPolicy> {
    demo_action_policy(path.strip_prefix(prefix)?, permission_prefix)
}

fn demo_action_policy(action: &str, permission_prefix: &'static str) -> Option<InfraPolicy> {
    let suffix = match action {
        "page" | "list" | "get" | "list-by-student-id" | "get-by-student-id" => "query",
        "create" => "create",
        "update" => "update",
        "delete" | "delete-list" => "delete",
        "export-excel" => "export",
        _ => return None,
    };
    let code = match (permission_prefix, suffix) {
        ("infra:demo01-contact", "query") => "infra:demo01-contact:query",
        ("infra:demo01-contact", "create") => "infra:demo01-contact:create",
        ("infra:demo01-contact", "update") => "infra:demo01-contact:update",
        ("infra:demo01-contact", "delete") => "infra:demo01-contact:delete",
        ("infra:demo01-contact", "export") => "infra:demo01-contact:export",
        ("infra:demo02-category", "query") => "infra:demo02-category:query",
        ("infra:demo02-category", "create") => "infra:demo02-category:create",
        ("infra:demo02-category", "update") => "infra:demo02-category:update",
        ("infra:demo02-category", "delete") => "infra:demo02-category:delete",
        ("infra:demo02-category", "export") => "infra:demo02-category:export",
        ("infra:demo03-student", "query") => "infra:demo03-student:query",
        ("infra:demo03-student", "create") => "infra:demo03-student:create",
        ("infra:demo03-student", "update") => "infra:demo03-student:update",
        ("infra:demo03-student", "delete") => "infra:demo03-student:delete",
        ("infra:demo03-student", "export") => "infra:demo03-student:export",
        _ => return None,
    };
    Some(InfraPolicy::Permission(code))
}

async fn capabilities() -> Json<ApiResponse<InfraCapability>> {
    Json(ApiResponse::new(InfraCapability::default()))
}

async fn config_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    page(
        &state.pool,
        "SELECT count(*) FROM infra_config WHERE deleted=0",
        "SELECT jsonb_build_object('id', id, 'category', category, 'type', type, 'name', name, 'key', config_key, 'value', value, 'visible', visible, 'remark', remark, 'createTime', create_time) FROM infra_config WHERE deleted=0 ORDER BY id DESC LIMIT $1 OFFSET $2",
        params,
    )
    .await
}

async fn config_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    get_one(&state.pool, "SELECT jsonb_build_object('id', id, 'category', category, 'type', type, 'name', name, 'key', config_key, 'value', value, 'visible', visible, 'remark', remark, 'createTime', create_time) FROM infra_config WHERE id=$1 AND deleted=0", id_param(&params)?).await
}

async fn config_value_by_key(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let key = params
        .get("key")
        .ok_or_else(|| AppError::bad_request("key is required"))?;
    let value = sqlx::query_scalar::<_, String>(
        "SELECT value FROM infra_config WHERE config_key=$1 AND deleted=0",
    )
    .bind(key)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to get config"))?
    .unwrap_or_default();
    Ok(Json(ApiResponse::new(value)))
}

async fn config_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let id = sqlx::query_scalar::<_, i64>("INSERT INTO infra_config (id, category, type, name, config_key, value, visible, remark) VALUES (nextval('infra_config_seq'),$1,$2,$3,$4,$5,$6,$7) RETURNING id")
        .bind(str_field(&payload, "category"))
        .bind(i16_field(&payload, "type", 2))
        .bind(str_field(&payload, "name"))
        .bind(str_field(&payload, "key"))
        .bind(str_field(&payload, "value"))
        .bind(bool_field(&payload, "visible", true))
        .bind(opt_str_field(&payload, "remark"))
        .fetch_one(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to create config"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn config_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    sqlx::query("UPDATE infra_config SET category=$2,type=$3,name=$4,config_key=$5,value=$6,visible=$7,remark=$8,update_time=now() WHERE id=$1 AND deleted=0")
        .bind(i64_field(&payload, "id", 0))
        .bind(str_field(&payload, "category"))
        .bind(i16_field(&payload, "type", 2))
        .bind(str_field(&payload, "name"))
        .bind(str_field(&payload, "key"))
        .bind(str_field(&payload, "value"))
        .bind(bool_field(&payload, "visible", true))
        .bind(opt_str_field(&payload, "remark"))
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to update config"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn config_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, "infra_config", id_param(&params)?).await
}

async fn config_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, "infra_config", ids_param(&params)).await
}

async fn data_source_list(
    State(state): State<InfraState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let rows = sqlx::query_scalar::<_, Value>("SELECT jsonb_build_object('id', id, 'name', name, 'url', url, 'username', username, 'password', CASE WHEN password IS NULL OR password = '' THEN '' ELSE '******' END, 'createTime', create_time) FROM infra_data_source_config WHERE deleted=0 ORDER BY id")
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to list data sources"))?;
    Ok(Json(ApiResponse::new(rows)))
}

async fn data_source_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    get_one(&state.pool, "SELECT jsonb_build_object('id', id, 'name', name, 'url', url, 'username', username, 'password', CASE WHEN password IS NULL OR password = '' THEN '' ELSE '******' END, 'createTime', create_time) FROM infra_data_source_config WHERE id=$1 AND deleted=0", id_param(&params)?).await
}

async fn data_source_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let password = seal_secret(&str_field(&payload, "password"))
        .map_err(|error| AppError::internal(error.to_string()))?;
    let id = sqlx::query_scalar::<_, i64>("INSERT INTO infra_data_source_config (id, name, url, username, password) VALUES (nextval('infra_data_source_config_seq'),$1,$2,$3,$4) RETURNING id")
        .bind(str_field(&payload, "name")).bind(str_field(&payload, "url")).bind(str_field(&payload, "username")).bind(password)
        .fetch_one(&state.pool).await.map_err(|_| AppError::internal("failed to create data source"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn data_source_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let password = opt_str_field(&payload, "password")
        .filter(|value| value != "******")
        .map(|value| seal_secret(&value))
        .transpose()
        .map_err(|error| AppError::internal(error.to_string()))?;
    sqlx::query("UPDATE infra_data_source_config SET name=$2,url=$3,username=$4,password=COALESCE($5,password),update_time=now() WHERE id=$1 AND deleted=0")
        .bind(i64_field(&payload, "id", 0)).bind(str_field(&payload, "name")).bind(str_field(&payload, "url")).bind(str_field(&payload, "username")).bind(password)
        .execute(&state.pool).await.map_err(|_| AppError::internal("failed to update data source"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn data_source_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, "infra_data_source_config", id_param(&params)?).await
}

async fn data_source_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, "infra_data_source_config", ids_param(&params)).await
}

async fn file_config_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    page(&state.pool, "SELECT count(*) FROM infra_file_config WHERE deleted=0", "SELECT jsonb_build_object('id', id, 'name', name, 'storage', storage, 'master', master, 'visible', true, 'config', config::jsonb, 'remark', remark, 'createTime', create_time) FROM infra_file_config WHERE deleted=0 ORDER BY id DESC LIMIT $1 OFFSET $2", params).await
}

async fn file_config_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    get_one(&state.pool, "SELECT jsonb_build_object('id', id, 'name', name, 'storage', storage, 'master', master, 'visible', true, 'config', config::jsonb, 'remark', remark, 'createTime', create_time) FROM infra_file_config WHERE id=$1 AND deleted=0", id_param(&params)?).await
}

async fn file_config_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let id = sqlx::query_scalar::<_, i64>("INSERT INTO infra_file_config (id, name, storage, master, config, remark) VALUES (nextval('infra_file_config_seq'),$1,$2,$3,$4,$5) RETURNING id")
        .bind(str_field(&payload, "name")).bind(i16_field(&payload, "storage", 10)).bind(bool_field(&payload, "master", false)).bind(payload.get("config").cloned().unwrap_or_else(|| json!({})).to_string()).bind(opt_str_field(&payload, "remark"))
        .fetch_one(&state.pool).await.map_err(|_| AppError::internal("failed to create file config"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn file_config_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    sqlx::query("UPDATE infra_file_config SET name=$2,storage=$3,master=$4,config=$5,remark=$6,update_time=now() WHERE id=$1 AND deleted=0")
        .bind(i64_field(&payload, "id", 0)).bind(str_field(&payload, "name")).bind(i16_field(&payload, "storage", 10)).bind(bool_field(&payload, "master", false)).bind(payload.get("config").cloned().unwrap_or_else(|| json!({})).to_string()).bind(opt_str_field(&payload, "remark"))
        .execute(&state.pool).await.map_err(|_| AppError::internal("failed to update file config"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn file_config_master(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = id_param(&params)?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to update file config"))?;
    sqlx::query("UPDATE infra_file_config SET master=false, update_time=now() WHERE deleted=0")
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to update file config"))?;
    sqlx::query(
        "UPDATE infra_file_config SET master=true, update_time=now() WHERE id=$1 AND deleted=0",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to update file config"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to update file config"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn file_config_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, "infra_file_config", id_param(&params)?).await
}

async fn file_config_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, "infra_file_config", ids_param(&params)).await
}

async fn file_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    page(&state.pool, "SELECT count(*) FROM infra_file WHERE deleted=0", "SELECT jsonb_build_object('id', id, 'configId', config_id, 'name', name, 'path', path, 'url', url, 'type', type, 'size', size, 'createTime', create_time) FROM infra_file WHERE deleted=0 ORDER BY id DESC LIMIT $1 OFFSET $2", params).await
}

async fn file_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let id = sqlx::query_scalar::<_, i64>("INSERT INTO infra_file (id, config_id, name, path, url, type, size) VALUES (nextval('infra_file_seq'),$1,$2,$3,$4,$5,$6) RETURNING id")
        .bind(opt_i64_field(&payload, "configId")).bind(opt_str_field(&payload, "name")).bind(str_field(&payload, "path")).bind(str_field(&payload, "url")).bind(opt_str_field(&payload, "type")).bind(i32_field(&payload, "size", 0))
        .fetch_one(&state.pool).await.map_err(|_| AppError::internal("failed to create file"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn file_upload(
    State(state): State<InfraState>,
    mut multipart: Multipart,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let field = multipart
        .next_field()
        .await
        .map_err(upload_multipart_error)?
        .ok_or_else(|| AppError::bad_request("file is required"))?;
    let name = field
        .file_name()
        .map(sanitize_file_name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("upload-{}", Utc::now().timestamp_millis()));
    let content_type = field
        .content_type()
        .map(ToString::to_string)
        .unwrap_or_else(|| "application/octet-stream".to_owned());
    let bytes = field
        .bytes()
        .await
        .map_err(upload_multipart_error)?
        .to_vec();
    if bytes.len() > state.upload_max_bytes {
        return Err(upload_too_large_error());
    }
    let date = Utc::now().format("%Y%m%d").to_string();
    let object_name = format!("{}_{}", Uuid::new_v4().simple(), name);
    let relative_path = format!("{date}/{object_name}");
    let object_key = format!("infra/{relative_path}");
    object_storage::put(&object_key, bytes.clone())
        .await
        .map_err(|_| AppError::internal("failed to save upload file to object storage"))?;
    let path = format!("/upload/{relative_path}");
    let size = i32::try_from(bytes.len()).unwrap_or(i32::MAX);
    let id = sqlx::query_scalar::<_, i64>("INSERT INTO infra_file (id, name, path, url, type, size) VALUES (nextval('infra_file_seq'),$1,$2,$3,$4,$5) RETURNING id")
        .bind(&name).bind(&path).bind(&path).bind(&content_type).bind(size)
        .fetch_one(&state.pool).await.map_err(|_| AppError::internal("failed to upload file"))?;
    Ok(Json(ApiResponse::new(
        json!({"id": id, "name": name, "path": path, "url": path, "type": content_type, "size": size}),
    )))
}

fn upload_multipart_error(error: MultipartError) -> AppError {
    if error.status() == StatusCode::PAYLOAD_TOO_LARGE {
        upload_too_large_error()
    } else {
        AppError::bad_request("failed to read upload file")
    }
}

fn upload_too_large_error() -> AppError {
    AppError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        StatusCode::PAYLOAD_TOO_LARGE.as_u16(),
        "upload exceeds the configured size limit",
    )
}

async fn file_download(Path(path): Path<String>) -> Result<Response, AppError> {
    if path.split('/').any(|part| part == ".." || part.is_empty()) {
        return Err(AppError::bad_request("invalid file path"));
    }
    let bytes = object_storage::get(&format!("infra/{path}"))
        .await
        .map_err(|_| AppError::not_found("file not found"))?;
    build_file_download_response(&path, bytes)
}

fn build_file_download_response(path: &str, bytes: Vec<u8>) -> Result<Response, AppError> {
    let content_type = infer_content_type(path);
    let file_name = path
        .rsplit('/')
        .next()
        .map(sanitize_file_name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "download".to_owned());
    let disposition = if is_safe_inline_image(path) {
        format!("inline; filename=\"{file_name}\"")
    } else {
        format!("attachment; filename=\"{file_name}\"")
    };
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, disposition)
        .header("x-content-type-options", "nosniff")
        .header("content-security-policy", "default-src 'none'; sandbox")
        .body(Body::from(bytes))
        .map_err(|_| AppError::internal("failed to read file"))
}

fn configured_upload_max_bytes() -> usize {
    parse_upload_max_bytes(env::var("INFRA_UPLOAD_MAX_BYTES").ok().as_deref())
}

fn parse_upload_max_bytes(value: Option<&str>) -> usize {
    value
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_UPLOAD_MAX_BYTES)
        .min(MAX_CONFIGURABLE_UPLOAD_BYTES)
}

fn sanitize_file_name(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '-' | '_' => ch,
            _ => '_',
        })
        .collect()
}

fn infer_content_type(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "txt" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "pdf" => "application/pdf",
        "html" => "text/html; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn is_safe_inline_image(path: &str) -> bool {
    matches!(
        path.rsplit('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp"
    )
}

async fn file_presigned_url(
    Query(params): Query<HashMap<String, String>>,
) -> Json<ApiResponse<Value>> {
    let name = params.get("name").cloned().unwrap_or_else(|| "file".into());
    let directory = params
        .get("directory")
        .cloned()
        .unwrap_or_else(|| "upload".into());
    let path = format!("/{directory}/{name}");
    Json(ApiResponse::new(
        json!({"configId": 1, "uploadUrl": path, "url": path, "path": path}),
    ))
}

async fn file_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, "infra_file", id_param(&params)?).await
}

async fn file_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, "infra_file", ids_param(&params)).await
}

async fn job_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    page(&state.pool, "SELECT count(*) FROM infra_job WHERE deleted=0", "SELECT jsonb_build_object('id', id, 'name', name, 'status', status, 'handlerName', handler_name, 'handlerParam', handler_param, 'cronExpression', cron_expression, 'retryCount', retry_count, 'retryInterval', retry_interval, 'monitorTimeout', monitor_timeout, 'createTime', create_time) FROM infra_job WHERE deleted=0 ORDER BY id DESC LIMIT $1 OFFSET $2", params).await
}

async fn job_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    get_one(&state.pool, "SELECT jsonb_build_object('id', id, 'name', name, 'status', status, 'handlerName', handler_name, 'handlerParam', handler_param, 'cronExpression', cron_expression, 'retryCount', retry_count, 'retryInterval', retry_interval, 'monitorTimeout', monitor_timeout, 'createTime', create_time) FROM infra_job WHERE id=$1 AND deleted=0", id_param(&params)?).await
}

async fn job_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let expression = str_field(&payload, "cronExpression");
    validate_cron(&expression).map_err(AppError::bad_request)?;
    let status = INFRA_JOB_STATUS_NORMAL;
    let next_run_at = (status == INFRA_JOB_STATUS_NORMAL)
        .then(|| next_occurrence(&expression, Utc::now()))
        .transpose()
        .map_err(AppError::bad_request)?;
    let id = sqlx::query_scalar::<_, i64>("INSERT INTO infra_job (id, name, status, handler_name, handler_param, cron_expression, retry_count, retry_interval, monitor_timeout, next_run_at) VALUES (nextval('infra_job_seq'),$1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING id")
        .bind(str_field(&payload, "name")).bind(status).bind(str_field(&payload, "handlerName")).bind(opt_str_field(&payload, "handlerParam")).bind(expression).bind(i32_field(&payload, "retryCount", 0)).bind(i32_field(&payload, "retryInterval", 0)).bind(i32_field(&payload, "monitorTimeout", 0)).bind(next_run_at)
        .fetch_one(&state.pool).await.map_err(|_| AppError::internal("failed to create job"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn job_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = i64_field(&payload, "id", 0);
    let current_status: i16 =
        sqlx::query_scalar("SELECT status FROM infra_job WHERE id=$1 AND deleted=0")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to read job"))?
            .ok_or_else(|| AppError::not_found("job not found"))?;
    let expression = str_field(&payload, "cronExpression");
    validate_cron(&expression).map_err(AppError::bad_request)?;
    let status = opt_i64_field(&payload, "status")
        .and_then(|value| i16::try_from(value).ok())
        .unwrap_or(current_status);
    let next_run_at = (status == INFRA_JOB_STATUS_NORMAL)
        .then(|| next_occurrence(&expression, Utc::now()))
        .transpose()
        .map_err(AppError::bad_request)?;
    sqlx::query("UPDATE infra_job SET name=$2,status=$3,handler_name=$4,handler_param=$5,cron_expression=$6,retry_count=$7,retry_interval=$8,monitor_timeout=$9,next_run_at=$10,update_time=now() WHERE id=$1 AND deleted=0")
        .bind(id).bind(str_field(&payload, "name")).bind(status).bind(str_field(&payload, "handlerName")).bind(opt_str_field(&payload, "handlerParam")).bind(expression).bind(i32_field(&payload, "retryCount", 0)).bind(i32_field(&payload, "retryInterval", 0)).bind(i32_field(&payload, "monitorTimeout", 0)).bind(next_run_at)
        .execute(&state.pool).await.map_err(|_| AppError::internal("failed to update job"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn job_update_status(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
    body: Bytes,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let payload = parse_optional_json_body(&body)?;
    let id = params
        .get("id")
        .and_then(|value| value.parse::<i64>().ok())
        .or_else(|| {
            payload
                .as_ref()
                .and_then(|value| opt_i64_field(value, "id"))
        })
        .ok_or_else(|| AppError::bad_request("id is required"))?;
    let status = params
        .get("status")
        .and_then(|value| value.parse::<i16>().ok())
        .or_else(|| {
            payload
                .as_ref()
                .and_then(|value| opt_i64_field(value, "status"))
                .and_then(|value| i16::try_from(value).ok())
        })
        .ok_or_else(|| AppError::bad_request("status is required"))?;
    if !matches!(status, 1 | 2) {
        return Err(AppError::bad_request("status must be 1 or 2"));
    }
    let expression: String =
        sqlx::query_scalar("SELECT cron_expression FROM infra_job WHERE id=$1 AND deleted=0")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to read job"))?
            .ok_or_else(|| AppError::not_found("job not found"))?;
    let next_run_at = (status == INFRA_JOB_STATUS_NORMAL)
        .then(|| next_occurrence(&expression, Utc::now()))
        .transpose()
        .map_err(AppError::bad_request)?;
    sqlx::query(
        "UPDATE infra_job SET status=$2,next_run_at=$3,update_time=now() WHERE id=$1 AND deleted=0",
    )
    .bind(id)
    .bind(status)
    .bind(next_run_at)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update job status"))?;
    Ok(Json(ApiResponse::new(())))
}

fn parse_optional_json_body(body: &Bytes) -> Result<Option<Value>, AppError> {
    if body.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(body)
        .map(Some)
        .map_err(|_| AppError::bad_request("invalid json body"))
}

async fn job_trigger(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = id_param(&params)?;
    let job: Option<(String, Option<String>, i32, i32, i32)> = sqlx::query_as(
        "SELECT handler_name,handler_param,retry_count,retry_interval,monitor_timeout FROM infra_job WHERE id=$1 AND deleted=0",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to read job"))?;
    let (handler_name, handler_param, retry_count, retry_interval, monitor_timeout) =
        job.ok_or_else(|| AppError::not_found("job not found"))?;
    enqueue_infra_job(
        &state.pool,
        ScheduledJobPayload {
            infra_job_id: id,
            handler_name,
            handler_param,
            scheduled_at: Utc::now(),
            triggered_by: "manual".to_string(),
            retry_interval_millis: u64::try_from(retry_interval).unwrap_or_default(),
            monitor_timeout_millis: u64::try_from(monitor_timeout).unwrap_or_default(),
        },
        retry_count,
    )
    .await?;
    Ok(Json(ApiResponse::new(())))
}

async fn job_next_times(
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<String>>>, AppError> {
    let expression = params
        .get("cronExpression")
        .or_else(|| params.get("cron"))
        .map(String::as_str)
        .unwrap_or("0 0/5 * * * ?");
    let values = next_occurrences(expression, Utc::now(), 5)
        .map_err(AppError::bad_request)?
        .into_iter()
        .map(|value| value.to_rfc3339())
        .collect();
    Ok(Json(ApiResponse::new(values)))
}

async fn job_sync(State(state): State<InfraState>) -> Result<Json<ApiResponse<i64>>, AppError> {
    let jobs: Vec<(i64, String)> =
        sqlx::query_as("SELECT id,cron_expression FROM infra_job WHERE deleted=0 AND status=1")
            .fetch_all(&state.pool)
            .await
            .map_err(|_| AppError::internal("failed to sync jobs"))?;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to sync jobs"))?;
    for (id, expression) in &jobs {
        let next = next_occurrence(expression, Utc::now()).map_err(AppError::bad_request)?;
        sqlx::query("UPDATE infra_job SET next_run_at=$2,update_time=now() WHERE id=$1")
            .bind(id)
            .bind(next)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::internal("failed to sync jobs"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to sync jobs"))?;
    Ok(Json(ApiResponse::new(jobs.len() as i64)))
}

async fn enqueue_infra_job(
    pool: &PgPool,
    payload: ScheduledJobPayload,
    retry_count: i32,
) -> Result<i64, AppError> {
    let payload_json = serde_json::to_value(&payload)
        .map_err(|_| AppError::internal("failed to encode scheduled job"))?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to trigger job"))?;
    let task_id: i64 = sqlx::query_scalar(
        "INSERT INTO toonflow.tasks
         (project_id,task_class,related_objects,model,description,state,start_time)
         VALUES(NULL,'infraJob',$1,'rust-worker',$2,'running',
                (extract(epoch from clock_timestamp())*1000)::bigint)
         RETURNING id",
    )
    .bind(json!({"infraJobId": payload.infra_job_id}).to_string())
    .bind(format!("执行定时任务 {}", payload.handler_name))
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to create scheduled task"))?;
    sqlx::query(
        "INSERT INTO toonflow.distributed_jobs
         (message_id,task_id,kind,trace_id,trace_context,payload,max_attempts)
         VALUES($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::new_v4())
    .bind(task_id)
    .bind(INFRA_SCHEDULED_JOB_KIND)
    .bind(current_trace_id().unwrap_or_else(|| format!("infra-job-{task_id}")))
    .bind(json!(current_trace_context()))
    .bind(payload_json)
    .bind(retry_count.clamp(0, 99) + 1)
    .execute(&mut *tx)
    .await
    .map_err(|_| AppError::internal("failed to enqueue scheduled job"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to trigger job"))?;
    Ok(task_id)
}

async fn job_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, "infra_job", id_param(&params)?).await
}

async fn job_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, "infra_job", ids_param(&params)).await
}

async fn job_log_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    page(&state.pool, "SELECT count(*) FROM infra_job_log WHERE deleted=0", "SELECT jsonb_build_object('id', id, 'jobId', job_id, 'handlerName', handler_name, 'handlerParam', handler_param, 'executeIndex', execute_index, 'beginTime', begin_time, 'endTime', end_time, 'duration', duration, 'status', status, 'result', result, 'createTime', create_time) FROM infra_job_log WHERE deleted=0 ORDER BY id DESC LIMIT $1 OFFSET $2", params).await
}

async fn api_access_log_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    let page_no = params.page_no.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(10).clamp(1, 200);
    let offset = (page_no - 1) * page_size;
    let mut count = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT count(*) FROM infra_api_access_log WHERE deleted=0",
    );
    let mut list = QueryBuilder::<sqlx::Postgres>::new(
        "SELECT jsonb_build_object('id', id, 'traceId', trace_id, 'userId', user_id, 'userType', user_type, 'applicationName', application_name, 'requestMethod', request_method, 'requestUrl', request_url, 'requestParams', request_params, 'responseBody', response_body, 'userIp', user_ip, 'userAgent', user_agent, 'operateModule', operate_module, 'operateName', operate_name, 'operateType', operate_type, 'beginTime', begin_time, 'endTime', end_time, 'duration', duration, 'resultCode', result_code, 'resultMsg', result_msg, 'createTime', create_time) FROM infra_api_access_log WHERE deleted=0",
    );
    if let Some(value) = params.user_id { count.push(" AND user_id=").push_bind(value); list.push(" AND user_id=").push_bind(value); }
    if let Some(value) = params.user_type { count.push(" AND user_type=").push_bind(value); list.push(" AND user_type=").push_bind(value); }
    if let Some(value) = params.application_name.as_deref().filter(|value| !value.is_empty()) { count.push(" AND application_name ILIKE ").push_bind(format!("%{value}%")); list.push(" AND application_name ILIKE ").push_bind(format!("%{value}%")); }
    if let Some(value) = params.duration { count.push(" AND duration=").push_bind(value); list.push(" AND duration=").push_bind(value); }
    if let Some(value) = params.result_code { count.push(" AND result_code=").push_bind(value); list.push(" AND result_code=").push_bind(value); }
    let total: i64 = count.build_query_scalar().fetch_one(&state.pool).await.map_err(|_| AppError::internal("failed to count records"))?;
    let list = list.push(" ORDER BY id DESC LIMIT ").push_bind(page_size).push(" OFFSET ").push_bind(offset).build_query_scalar::<Value>().fetch_all(&state.pool).await.map_err(|_| AppError::internal("failed to list records"))?;
    Ok(Json(ApiResponse::new(Page { list, total })))
}

async fn api_error_log_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    page(&state.pool, "SELECT count(*) FROM infra_api_error_log WHERE deleted=0", "SELECT jsonb_build_object('id', id, 'traceId', trace_id, 'userId', user_id, 'userType', user_type, 'applicationName', application_name, 'requestMethod', request_method, 'requestUrl', request_url, 'requestParams', request_params, 'userIp', user_ip, 'userAgent', user_agent, 'exceptionTime', exception_time, 'exceptionName', exception_name, 'exceptionMessage', exception_message, 'exceptionRootCauseMessage', exception_root_cause_message, 'exceptionStackTrace', exception_stack_trace, 'exceptionClassName', exception_class_name, 'exceptionFileName', exception_file_name, 'exceptionMethodName', exception_method_name, 'exceptionLineNumber', exception_line_number, 'processStatus', process_status, 'processTime', process_time, 'processUserId', process_user_id, 'createTime', create_time) FROM infra_api_error_log WHERE deleted=0 ORDER BY id DESC LIMIT $1 OFFSET $2", params).await
}

async fn api_error_log_update_status(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    sqlx::query("UPDATE infra_api_error_log SET process_status=$2, process_time=now(), update_time=now() WHERE id=$1")
        .bind(id_param(&params)?)
        .bind(params.get("processStatus").and_then(|value| value.parse::<i16>().ok()).unwrap_or(1))
        .execute(&state.pool).await.map_err(|_| AppError::internal("failed to update error log"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn redis_monitor_info() -> Json<ApiResponse<Value>> {
    Json(ApiResponse::new(match redis_monitor_info_value().await {
        Ok(value) => value,
        Err(error) => json!({
            "available": false,
            "error": error,
            "info": {},
            "dbSize": 0,
            "commandStats": []
        }),
    }))
}

async fn redis_monitor_info_value() -> Result<Value, String> {
    let url = env::var("REDIS_URL").map_err(|_| "REDIS_URL is not configured".to_owned())?;
    let client = redis::Client::open(url.as_str()).map_err(|error| error.to_string())?;
    let mut connection = client
        .get_multiplexed_async_connection()
        .await
        .map_err(|error| error.to_string())?;
    let info_text: String = redis::cmd("INFO")
        .query_async(&mut connection)
        .await
        .map_err(|error| error.to_string())?;
    let db_size: i64 = redis::cmd("DBSIZE")
        .query_async(&mut connection)
        .await
        .map_err(|error| error.to_string())?;
    // Default INFO does not include the commandstats section.
    let command_text: String = redis::cmd("INFO")
        .arg("commandstats")
        .query_async(&mut connection)
        .await
        .map_err(|error| error.to_string())?;
    let mut info = serde_json::Map::new();
    let mut command_stats = Vec::new();
    for line in info_text.lines().chain(command_text.lines()) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if let Some(command) = key.strip_prefix("cmdstat_") {
            let calls = value
                .split(',')
                .find_map(|part| part.strip_prefix("calls="))
                .and_then(|calls| calls.parse::<i64>().ok())
                .unwrap_or(0);
            let usec = value
                .split(',')
                .find_map(|part| part.strip_prefix("usec="))
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(0);
            command_stats.push(json!({"command": command, "calls": calls, "usec": usec}));
        } else {
            info.insert(key.to_owned(), json!(value));
        }
    }
    Ok(json!({
        "available": true,
        "info": info,
        "dbSize": db_size,
        "commandStats": command_stats
    }))
}

#[derive(Clone, Copy)]
struct TableSpec {
    table: &'static str,
    seq: &'static str,
}

const CODEGEN_TABLE: TableSpec = TableSpec {
    table: "infra_codegen_table",
    seq: "infra_codegen_table_seq",
};
const CODEGEN_COLUMN: TableSpec = TableSpec {
    table: "infra_codegen_column",
    seq: "infra_codegen_column_seq",
};
const DEMO01_CONTACT: TableSpec = TableSpec {
    table: "yudao_demo01_contact",
    seq: "yudao_demo01_contact_seq",
};
const DEMO02_CATEGORY: TableSpec = TableSpec {
    table: "yudao_demo02_category",
    seq: "yudao_demo02_category_seq",
};
const DEMO03_STUDENT: TableSpec = TableSpec {
    table: "yudao_demo03_student",
    seq: "yudao_demo03_student_seq",
};
const DEMO03_COURSE: TableSpec = TableSpec {
    table: "yudao_demo03_course",
    seq: "yudao_demo03_course_seq",
};
const DEMO03_GRADE: TableSpec = TableSpec {
    table: "yudao_demo03_grade",
    seq: "yudao_demo03_grade_seq",
};

async fn codegen_table_list(
    State(state): State<InfraState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    table_list(&state.pool, CODEGEN_TABLE).await
}

async fn codegen_table_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    table_page(&state.pool, CODEGEN_TABLE, params).await
}

async fn codegen_detail(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let table_id = params
        .get("tableId")
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("tableId is required"))?;
    let table = table_get_value(&state.pool, CODEGEN_TABLE, table_id).await?;
    let columns = sqlx::query_scalar::<_, Value>(
        "SELECT to_jsonb(t) FROM infra_codegen_column t WHERE table_id=$1 AND deleted=0 ORDER BY ordinal_position, id",
    )
    .bind(table_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list codegen columns"))?
    .into_iter()
    .map(table_value)
    .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(
        json!({ "table": table, "columns": columns }),
    )))
}

async fn codegen_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    if let Some(table) = payload.get("table").cloned() {
        let _ = table_update(&state.pool, CODEGEN_TABLE, table).await?;
    }
    for column in payload
        .get("columns")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let _ = table_update(&state.pool, CODEGEN_COLUMN, column).await?;
    }
    Ok(Json(ApiResponse::new(())))
}

async fn codegen_preview(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let table_id = table_id_param(&params)?;
    Ok(Json(ApiResponse::new(
        codegen_preview_files(&state.pool, table_id).await?,
    )))
}

async fn codegen_download(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, AppError> {
    let table_id = table_id_param(&params)?;
    let files = codegen_preview_files(&state.pool, table_id).await?;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for file in files {
        let path = file["filePath"].as_str().unwrap_or("generated.txt");
        let code = file["code"].as_str().unwrap_or_default();
        writer
            .start_file(path, options)
            .map_err(|_| AppError::internal("failed to build codegen archive"))?;
        writer
            .write_all(code.as_bytes())
            .map_err(|_| AppError::internal("failed to build codegen archive"))?;
    }
    let bytes = writer
        .finish()
        .map_err(|_| AppError::internal("failed to build codegen archive"))?
        .into_inner();
    Response::builder()
        .header("content-type", "application/zip")
        .header(
            "content-disposition",
            "attachment; filename=\"codegen.zip\"",
        )
        .body(Body::from(bytes))
        .map_err(|_| AppError::internal("failed to build download"))
}

async fn codegen_sync_from_db(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let table_id = table_id_param(&params)?;
    sync_codegen_columns(&state.pool, table_id).await?;
    Ok(Json(ApiResponse::new(())))
}

async fn codegen_db_table_list(
    State(state): State<InfraState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let rows = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object('name', table_name, 'comment', table_name)
         FROM information_schema.tables
         WHERE table_schema='public'
           AND table_type='BASE TABLE'
           AND table_name NOT LIKE 'qrtz_%'
         ORDER BY table_name",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list database tables"))?;
    Ok(Json(ApiResponse::new(rows)))
}

async fn codegen_create_list(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let data_source_config_id = opt_i64_field(&payload, "dataSourceConfigId").unwrap_or(0);
    for table_name in payload
        .get("tableNames")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let exists = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM infra_codegen_table WHERE table_name=$1 AND deleted=0",
        )
        .bind(table_name)
        .fetch_one(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to check codegen table"))?;
        if exists > 0 {
            continue;
        }
        let business_name = table_name.rsplit('_').next().unwrap_or(table_name);
        let class_name = pascal_case(table_name);
        let table_id = sqlx::query_scalar::<_, i64>(
            "INSERT INTO infra_codegen_table (id, data_source_config_id, scene, table_name, table_comment, module_name, business_name, class_name, class_comment, author, template_type, front_type)
             VALUES (nextval('infra_codegen_table_seq'),$1,1,$2,$2,'infra',$3,$4,$2,'admin',1,20) RETURNING id",
        )
        .bind(data_source_config_id)
        .bind(table_name)
        .bind(business_name)
        .bind(class_name)
        .fetch_one(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to create codegen table"))?;

        insert_codegen_columns(&state.pool, table_id, table_name).await?;
    }
    Ok(Json(ApiResponse::new(())))
}

async fn codegen_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let table_id = params
        .get("tableId")
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("tableId is required"))?;
    soft_delete(&state.pool, "infra_codegen_table", table_id).await
}

async fn codegen_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(
        &state.pool,
        "infra_codegen_table",
        ids_named_param(&params, "tableIds"),
    )
    .await
}

fn table_id_param(params: &HashMap<String, String>) -> Result<i64, AppError> {
    params
        .get("tableId")
        .or_else(|| params.get("id"))
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("tableId is required"))
}

async fn codegen_preview_files(pool: &PgPool, table_id: i64) -> Result<Vec<Value>, AppError> {
    let table = table_get_value(pool, CODEGEN_TABLE, table_id).await?;
    let class_name = table["className"].as_str().unwrap_or("Generated");
    Ok(vec![
        json!({"filePath": format!("src/api/{class_name}.ts"), "code": format!("// preview for {class_name}\nexport interface {class_name} {{\n  id: number;\n}}\n")}),
        json!({"filePath": format!("src/views/{class_name}/index.vue"), "code": format!("<template>\n  <div>{class_name}</div>\n</template>\n")}),
    ])
}

async fn sync_codegen_columns(pool: &PgPool, table_id: i64) -> Result<(), AppError> {
    let table = table_get_value(pool, CODEGEN_TABLE, table_id).await?;
    let table_name = table["tableName"]
        .as_str()
        .ok_or_else(|| AppError::bad_request("tableName is required"))?;
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to sync codegen columns"))?;
    sqlx::query("UPDATE infra_codegen_column SET deleted=1, update_time=now() WHERE table_id=$1")
        .bind(table_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AppError::internal("failed to sync codegen columns"))?;
    insert_codegen_columns_in_tx(&mut transaction, table_id, table_name).await?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to sync codegen columns"))?;
    Ok(())
}

async fn insert_codegen_columns(
    pool: &PgPool,
    table_id: i64,
    table_name: &str,
) -> Result<(), AppError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to create codegen column"))?;
    insert_codegen_columns_in_tx(&mut transaction, table_id, table_name).await?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to create codegen column"))?;
    Ok(())
}

async fn insert_codegen_columns_in_tx(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    table_id: i64,
    table_name: &str,
) -> Result<(), AppError> {
    let columns = sqlx::query_as::<_, (String, String, bool, i32)>(
        "SELECT column_name, data_type, is_nullable='YES', ordinal_position::int
         FROM information_schema.columns
         WHERE table_schema='public' AND table_name=$1
         ORDER BY ordinal_position",
    )
    .bind(table_name)
    .fetch_all(&mut **transaction)
    .await
    .map_err(|_| AppError::internal("failed to inspect database table"))?;
    for (column_name, data_type, nullable, ordinal_position) in columns {
        sqlx::query(
            "INSERT INTO infra_codegen_column (id, table_id, column_name, data_type, column_comment, nullable, primary_key, ordinal_position, java_type, java_field, create_operation, update_operation, list_operation, list_operation_result, html_type)
             VALUES (nextval('infra_codegen_column_seq'),$1,$2,$3,$2,$4,$5,$6,$7,$8,true,true,true,true,$9)",
        )
        .bind(table_id)
        .bind(&column_name)
        .bind(&data_type)
        .bind(nullable)
        .bind(column_name == "id")
        .bind(ordinal_position)
        .bind(java_type(&data_type))
        .bind(snake_to_camel(&column_name))
        .bind(html_type(&data_type))
        .execute(&mut **transaction)
        .await
        .map_err(|_| AppError::internal("failed to create codegen column"))?;
    }
    Ok(())
}

async fn demo01_contact_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    table_page(&state.pool, DEMO01_CONTACT, params).await
}

async fn demo01_contact_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    table_get(&state.pool, DEMO01_CONTACT, id_param(&params)?).await
}

async fn demo01_contact_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    table_create(&state.pool, DEMO01_CONTACT, payload).await
}

async fn demo01_contact_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    table_update(&state.pool, DEMO01_CONTACT, payload).await
}

async fn demo01_contact_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, DEMO01_CONTACT.table, id_param(&params)?).await
}

async fn demo01_contact_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, DEMO01_CONTACT.table, ids_param(&params)).await
}

async fn demo02_category_list(
    State(state): State<InfraState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    table_list(&state.pool, DEMO02_CATEGORY).await
}

async fn demo02_category_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    table_get(&state.pool, DEMO02_CATEGORY, id_param(&params)?).await
}

async fn demo02_category_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    table_create(&state.pool, DEMO02_CATEGORY, payload).await
}

async fn demo02_category_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    table_update(&state.pool, DEMO02_CATEGORY, payload).await
}

async fn demo02_category_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, DEMO02_CATEGORY.table, id_param(&params)?).await
}

async fn demo03_student_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    table_page(&state.pool, DEMO03_STUDENT, params).await
}

async fn demo03_student_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    table_get(&state.pool, DEMO03_STUDENT, id_param(&params)?).await
}

async fn demo03_student_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    table_create(&state.pool, DEMO03_STUDENT, payload).await
}

async fn demo03_student_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    table_update(&state.pool, DEMO03_STUDENT, payload).await
}

async fn demo03_student_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, DEMO03_STUDENT.table, id_param(&params)?).await
}

async fn demo03_student_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, DEMO03_STUDENT.table, ids_param(&params)).await
}

async fn demo03_course_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    table_page(&state.pool, DEMO03_COURSE, params).await
}

async fn demo03_course_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    table_get(&state.pool, DEMO03_COURSE, id_param(&params)?).await
}

async fn demo03_course_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    table_create(&state.pool, DEMO03_COURSE, payload).await
}

async fn demo03_course_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    table_update(&state.pool, DEMO03_COURSE, payload).await
}

async fn demo03_course_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, DEMO03_COURSE.table, id_param(&params)?).await
}

async fn demo03_course_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, DEMO03_COURSE.table, ids_param(&params)).await
}

async fn demo03_grade_page(
    State(state): State<InfraState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    table_page(&state.pool, DEMO03_GRADE, params).await
}

async fn demo03_grade_get(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    table_get(&state.pool, DEMO03_GRADE, id_param(&params)?).await
}

async fn demo03_grade_create(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    table_create(&state.pool, DEMO03_GRADE, payload).await
}

async fn demo03_grade_update(
    State(state): State<InfraState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    table_update(&state.pool, DEMO03_GRADE, payload).await
}

async fn demo03_grade_delete(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete(&state.pool, DEMO03_GRADE.table, id_param(&params)?).await
}

async fn demo03_grade_delete_list(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    soft_delete_list(&state.pool, DEMO03_GRADE.table, ids_param(&params)).await
}

async fn demo03_course_list_by_student_id(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    table_list_by_i64(
        &state.pool,
        DEMO03_COURSE,
        "student_id",
        id_named_param(&params, "studentId")?,
    )
    .await
}

async fn demo03_grade_get_by_student_id(
    State(state): State<InfraState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let student_id = id_named_param(&params, "studentId")?;
    let sql = "SELECT to_jsonb(t) FROM yudao_demo03_grade t WHERE student_id=$1 AND deleted=0 ORDER BY id DESC LIMIT 1";
    let value = sqlx::query_scalar::<_, Value>(sql)
        .bind(student_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to get record"))?
        .map(table_value)
        .unwrap_or_else(|| json!({}));
    Ok(Json(ApiResponse::new(value)))
}

async fn table_page(
    pool: &PgPool,
    spec: TableSpec,
    params: QueryParams,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    let page_no = params.page_no.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(10).clamp(1, 200);
    let offset = (page_no - 1) * page_size;
    let total_sql = format!("SELECT count(*) FROM {} WHERE deleted=0", spec.table);
    let total = sqlx::query_scalar::<_, i64>(&total_sql)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to count records"))?;
    let list_sql = format!(
        "SELECT to_jsonb(t) FROM {} t WHERE deleted=0 ORDER BY id DESC LIMIT $1 OFFSET $2",
        spec.table
    );
    let list = sqlx::query_scalar::<_, Value>(&list_sql)
        .bind(page_size)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?
        .into_iter()
        .map(table_value)
        .collect();
    Ok(Json(ApiResponse::new(Page { list, total })))
}

async fn table_list(
    pool: &PgPool,
    spec: TableSpec,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let sql = format!(
        "SELECT to_jsonb(t) FROM {} t WHERE deleted=0 ORDER BY id",
        spec.table
    );
    let list = sqlx::query_scalar::<_, Value>(&sql)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?
        .into_iter()
        .map(table_value)
        .collect();
    Ok(Json(ApiResponse::new(list)))
}

async fn table_list_by_i64(
    pool: &PgPool,
    spec: TableSpec,
    column: &str,
    value: i64,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let sql = format!(
        "SELECT to_jsonb(t) FROM {} t WHERE {column}=$1 AND deleted=0 ORDER BY id",
        spec.table
    );
    let list = sqlx::query_scalar::<_, Value>(&sql)
        .bind(value)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?
        .into_iter()
        .map(table_value)
        .collect();
    Ok(Json(ApiResponse::new(list)))
}

async fn table_get(
    pool: &PgPool,
    spec: TableSpec,
    id: i64,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    Ok(Json(ApiResponse::new(
        table_get_value(pool, spec, id).await?,
    )))
}

async fn table_get_value(pool: &PgPool, spec: TableSpec, id: i64) -> Result<Value, AppError> {
    let sql = format!(
        "SELECT to_jsonb(t) FROM {} t WHERE id=$1 AND deleted=0",
        spec.table
    );
    let value = sqlx::query_scalar::<_, Value>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|_| AppError::internal("failed to get record"))?
        .ok_or_else(|| AppError::not_found("record not found"))?;
    Ok(table_value(value))
}

async fn table_create(
    pool: &PgPool,
    spec: TableSpec,
    payload: Value,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let db_payload = camel_payload_to_snake(payload);
    let columns = table_writable_columns(pool, spec.table, &db_payload, false).await?;
    if columns.is_empty() {
        return Err(AppError::bad_request("no writable fields"));
    }
    let column_sql = columns.join(", ");
    let record_sql = columns
        .iter()
        .map(|column| format!("r.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "INSERT INTO {} (id, {}) SELECT nextval('{}'), {} FROM jsonb_populate_record(NULL::{}, $1::jsonb) AS r RETURNING id",
        spec.table, column_sql, spec.seq, record_sql, spec.table
    );
    let id = sqlx::query_scalar::<_, i64>(&sql)
        .bind(db_payload)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to create record"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn table_update(
    pool: &PgPool,
    spec: TableSpec,
    payload: Value,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = i64_field(&payload, "id", 0);
    if id == 0 {
        return Err(AppError::bad_request("id is required"));
    }
    let db_payload = camel_payload_to_snake(payload);
    let columns = table_writable_columns(pool, spec.table, &db_payload, true).await?;
    if columns.is_empty() {
        return Ok(Json(ApiResponse::new(())));
    }
    let set_sql = columns
        .iter()
        .map(|column| format!("{column}=r.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE {} t SET {}, update_time=now() FROM jsonb_populate_record(NULL::{}, $2::jsonb) AS r WHERE t.id=$1 AND t.deleted=0",
        spec.table, set_sql, spec.table
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(db_payload)
        .execute(pool)
        .await
        .map_err(|_| AppError::internal("failed to update record"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn table_writable_columns(
    pool: &PgPool,
    table: &str,
    payload: &Value,
    include_id: bool,
) -> Result<Vec<String>, AppError> {
    let Some(object) = payload.as_object() else {
        return Ok(vec![]);
    };
    let columns = sqlx::query_scalar::<_, String>(
        "SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name=$1",
    )
    .bind(table)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to inspect table"))?;
    Ok(object
        .keys()
        .filter(|key| {
            (include_id || key.as_str() != "id")
                && !matches!(
                    key.as_str(),
                    "creator" | "create_time" | "updater" | "update_time" | "deleted" | "tenant_id"
                )
                && columns.iter().any(|column| column == *key)
        })
        .cloned()
        .collect())
}

async fn page(
    pool: &PgPool,
    count_sql: &str,
    list_sql: &str,
    params: QueryParams,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    let page_no = params.page_no.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(10).clamp(1, 200);
    let offset = (page_no - 1) * page_size;
    let total = sqlx::query_scalar::<_, i64>(count_sql)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to count records"))?;
    let list = sqlx::query_scalar::<_, Value>(list_sql)
        .bind(page_size)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?;
    Ok(Json(ApiResponse::new(Page { list, total })))
}

async fn get_one(pool: &PgPool, sql: &str, id: i64) -> Result<Json<ApiResponse<Value>>, AppError> {
    let value = sqlx::query_scalar::<_, Value>(sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|_| AppError::internal("failed to get record"))?
        .ok_or_else(|| AppError::not_found("record not found"))?;
    Ok(Json(ApiResponse::new(value)))
}

async fn soft_delete(
    pool: &PgPool,
    table: &str,
    id: i64,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let sql = format!("UPDATE {table} SET deleted=1, update_time=now() WHERE id=$1");
    sqlx::query(&sql)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|_| AppError::internal("failed to delete record"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn soft_delete_list(
    pool: &PgPool,
    table: &str,
    ids: Vec<i64>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    for id in ids {
        let _ = soft_delete(pool, table, id).await?;
    }
    Ok(Json(ApiResponse::new(())))
}

fn id_param(params: &HashMap<String, String>) -> Result<i64, AppError> {
    params
        .get("id")
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("id is required"))
}

fn ids_param(params: &HashMap<String, String>) -> Vec<i64> {
    ids_named_param(params, "ids")
}

fn ids_named_param(params: &HashMap<String, String>, name: &str) -> Vec<i64> {
    params
        .get(name)
        .into_iter()
        .flat_map(|ids| ids.split(','))
        .filter_map(|id| id.parse::<i64>().ok())
        .collect()
}

fn id_named_param(params: &HashMap<String, String>, name: &str) -> Result<i64, AppError> {
    params
        .get(name)
        .and_then(|value| value.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request(format!("{name} is required")))
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn opt_str_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn i16_field(value: &Value, key: &str, default: i16) -> i16 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i16::try_from(value).ok())
        .unwrap_or(default)
}

fn i32_field(value: &Value, key: &str, default: i32) -> i32 {
    value
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .unwrap_or(default)
}

fn i64_field(value: &Value, key: &str, default: i64) -> i64 {
    value
        .get(key)
        .and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
        })
        .unwrap_or(default)
}

fn opt_i64_field(value: &Value, key: &str) -> Option<i64> {
    value.get(key).and_then(|value| {
        value
            .as_i64()
            .or_else(|| value.as_str().and_then(|value| value.parse::<i64>().ok()))
    })
}

fn bool_field(value: &Value, key: &str, default: bool) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn table_value(value: Value) -> Value {
    let Value::Object(object) = value else {
        return value;
    };
    let mut mapped = serde_json::Map::new();
    for (key, value) in object {
        if key == "deleted" {
            continue;
        }
        mapped.insert(snake_to_camel(&key), parse_jsonish_value(value));
    }
    if mapped.contains_key("parentMenuId") {
        mapped.insert("isParentMenuIdValid".into(), Value::Bool(true));
    }
    Value::Object(mapped)
}

fn camel_payload_to_snake(value: Value) -> Value {
    let Value::Object(object) = value else {
        return json!({});
    };
    let mut mapped = serde_json::Map::new();
    for (key, value) in object {
        if matches!(
            key.as_str(),
            "createTime" | "updateTime" | "isParentMenuIdValid"
        ) {
            continue;
        }
        mapped.insert(camel_to_snake(&key), value);
    }
    Value::Object(mapped)
}

fn parse_jsonish_value(value: Value) -> Value {
    let Value::String(text) = &value else {
        return value;
    };
    if !(text.starts_with('{') || text.starts_with('[')) {
        return value;
    }
    serde_json::from_str(text).unwrap_or(value)
}

fn snake_to_camel(value: &str) -> String {
    let mut output = String::new();
    let mut uppercase = false;
    for character in value.chars() {
        if character == '_' {
            uppercase = true;
        } else if uppercase {
            output.push(character.to_ascii_uppercase());
            uppercase = false;
        } else {
            output.push(character);
        }
    }
    output
}

fn camel_to_snake(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character.is_ascii_uppercase() {
            output.push('_');
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }
    output
}

fn pascal_case(value: &str) -> String {
    let camel = snake_to_camel(value);
    let mut chars = camel.chars();
    match chars.next() {
        Some(first) => format!(
            "{}{}",
            first.to_ascii_uppercase(),
            chars.collect::<String>()
        ),
        None => "Generated".into(),
    }
}

fn java_type(data_type: &str) -> &'static str {
    match data_type {
        "bigint" => "Long",
        "integer" | "smallint" => "Integer",
        "boolean" => "Boolean",
        "timestamp without time zone" | "timestamp with time zone" => "LocalDateTime",
        "date" => "LocalDate",
        _ => "String",
    }
}

fn html_type(data_type: &str) -> &'static str {
    match data_type {
        "boolean" => "radio",
        "timestamp without time zone" | "timestamp with time zone" | "date" => "datetime",
        _ => "input",
    }
}

async fn ok_bool() -> Json<ApiResponse<bool>> {
    Json(ApiResponse::new(true))
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use axum::{
        body::Body,
        http::{Request, StatusCode, header},
    };
    use rust_toon_framework_security::{
        CurrentUser, DataScope, Permission, PermissionSet, SecurityConfig, TokenService,
    };
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    use super::{
        DEFAULT_UPLOAD_MAX_BYTES, InfraPolicy, InfraState, MAX_CONFIGURABLE_UPLOAD_BYTES,
        MULTIPART_OVERHEAD_BYTES, build_file_download_response, infra_policy,
        parse_upload_max_bytes, routes,
    };

    fn token_service() -> TokenService {
        TokenService::new(
            SecurityConfig::new(
                "infra-test-secret-with-at-least-thirty-two-bytes",
                "infra-test",
                "infra-test-client",
                Duration::from_secs(60),
            )
            .expect("valid test token configuration"),
        )
    }

    fn test_state(upload_max_bytes: usize) -> InfraState {
        InfraState {
            pool: PgPoolOptions::new()
                .connect_lazy("postgres://rust_toon:rust_toon@127.0.0.1/rust_toon_test")
                .expect("valid lazy test database URL"),
            tokens: token_service(),
            upload_max_bytes,
            started_at: Instant::now(),
        }
    }

    fn ordinary_user() -> CurrentUser {
        CurrentUser {
            user_id: "ordinary-user".into(),
            username: "ordinary".into(),
            tenant_id: Some("1".into()),
            role_codes: vec!["ordinary".into()],
            permissions: PermissionSet::default(),
            data_scope: DataScope::SelfOnly,
        }
    }

    fn upload_request(token: Option<&str>, body: Body) -> Request<Body> {
        let mut request = Request::builder()
            .method("POST")
            .uri("/infra/file/upload")
            .header(
                header::CONTENT_TYPE,
                "multipart/form-data; boundary=infra-test",
            );
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        request.body(body).expect("valid upload request")
    }

    #[test]
    fn every_registered_protected_route_has_a_policy() {
        // Read the route declarations themselves so a newly registered route
        // cannot silently be omitted from a manually duplicated test table.
        let source = include_str!("lib.rs");
        let protected_routes = source
            .split_once("let protected =")
            .expect("protected router declaration")
            .1
            .split_once(".route_layer(from_fn(authorize_infra))")
            .expect("authorization layer declaration")
            .0;

        let mut checked = 0;
        for route in protected_routes.split(".route(").skip(1) {
            let Some((_, quoted)) = route.split_once('"') else {
                continue;
            };
            let Some((path, _)) = quoted.split_once('"') else {
                continue;
            };
            if !path.starts_with("/infra/") {
                continue;
            }
            let policy = infra_policy(path)
                .unwrap_or_else(|| panic!("protected route has no access policy: {path}"));
            if let InfraPolicy::Permission(code) = policy {
                Permission::new(code)
                    .unwrap_or_else(|_| panic!("route has an invalid permission code: {path}"));
            }
            checked += 1;
        }

        assert!(checked >= 100, "unexpectedly parsed only {checked} routes");
    }

    #[test]
    fn sensitive_monitors_are_super_admin_only_but_upload_is_account_wide() {
        assert_eq!(
            infra_policy("/infra/monitor/postgresql"),
            Some(InfraPolicy::SuperAdmin)
        );
        assert_eq!(
            infra_policy("/infra/file/upload"),
            Some(InfraPolicy::Authenticated)
        );
        assert_eq!(
            infra_policy("/infra/file/presigned-url"),
            Some(InfraPolicy::Authenticated)
        );
        assert_eq!(
            infra_policy("/infra/file/create"),
            Some(InfraPolicy::Authenticated)
        );
        assert!(matches!(
            infra_policy("/infra/config/page"),
            Some(InfraPolicy::Permission("infra:config:query"))
        ));
        assert_eq!(infra_policy("/infra/not-registered"), None);
    }

    #[tokio::test]
    async fn authentication_runs_before_policy_and_ordinary_users_can_reach_upload() {
        let state = test_state(1024);
        let token = state
            .tokens
            .issue_access_token(ordinary_user())
            .expect("issue test access token");
        let app = routes(state);
        let empty_multipart = || Body::from("--infra-test--\r\n");

        let missing_credentials = app
            .clone()
            .oneshot(upload_request(None, empty_multipart()))
            .await
            .expect("upload response");
        assert_eq!(missing_credentials.status(), StatusCode::UNAUTHORIZED);

        // A valid multipart envelope with no file reaches the handler and is
        // rejected as a bad request. A 401/403 here would mean authentication
        // and authorization were stacked in the wrong order, or ordinary
        // account uploads had accidentally been made admin-only.
        let ordinary_upload = app
            .clone()
            .oneshot(upload_request(Some(&token), empty_multipart()))
            .await
            .expect("upload response");
        assert_eq!(ordinary_upload.status(), StatusCode::BAD_REQUEST);

        let forbidden_admin_route = app
            .oneshot(
                Request::builder()
                    .uri("/infra/config/page")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .expect("valid admin request"),
            )
            .await
            .expect("admin response");
        assert_eq!(forbidden_admin_route.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn multipart_envelope_is_bounded_before_the_upload_handler() {
        let state = test_state(8);
        let token = state
            .tokens
            .issue_access_token(ordinary_user())
            .expect("issue test access token");
        let app = routes(state);
        let file_too_large = "--infra-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"large.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n123456789\r\n--infra-test--\r\n".to_owned();
        let response = app
            .clone()
            .oneshot(upload_request(Some(&token), Body::from(file_too_large)))
            .await
            .expect("upload response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let oversized_envelope = "x".repeat(MULTIPART_OVERHEAD_BYTES + 32);
        let multipart = format!(
            "--infra-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"large.bin\"\r\nContent-Type: application/octet-stream\r\n\r\n{oversized_envelope}\r\n--infra-test--\r\n"
        );

        let response = app
            .oneshot(upload_request(Some(&token), Body::from(multipart)))
            .await
            .expect("upload response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn upload_limit_defaults_and_caps_invalid_configuration() {
        assert_eq!(parse_upload_max_bytes(None), DEFAULT_UPLOAD_MAX_BYTES);
        assert_eq!(parse_upload_max_bytes(Some("0")), DEFAULT_UPLOAD_MAX_BYTES);
        assert_eq!(
            parse_upload_max_bytes(Some("not-a-number")),
            DEFAULT_UPLOAD_MAX_BYTES
        );
        assert_eq!(parse_upload_max_bytes(Some("4096")), 4096);
        assert_eq!(
            parse_upload_max_bytes(Some("999999999999")),
            MAX_CONFIGURABLE_UPLOAD_BYTES
        );
    }

    #[test]
    fn active_download_types_are_forced_to_attachment_with_browser_guards() {
        for path in ["20260827/payload.html", "20260827/payload.svg"] {
            let response =
                build_file_download_response(path, b"payload".to_vec()).expect("download response");
            assert!(
                response.headers()[header::CONTENT_DISPOSITION]
                    .to_str()
                    .expect("valid disposition")
                    .starts_with("attachment;")
            );
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert_eq!(
                response.headers()["content-security-policy"],
                "default-src 'none'; sandbox"
            );
        }

        let image =
            build_file_download_response("20260827/preview.png", vec![]).expect("image response");
        assert!(
            image.headers()[header::CONTENT_DISPOSITION]
                .to_str()
                .expect("valid disposition")
                .starts_with("inline;")
        );
        assert_eq!(image.headers()[header::CONTENT_TYPE], "image/png");
    }
}
