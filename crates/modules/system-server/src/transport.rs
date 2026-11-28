use axum::{
    Json, Router,
    extract::{Query, State},
    middleware::from_fn_with_state,
    routing::{get, post},
};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::{CurrentUser, DataScope, authenticate};
use rust_toon_framework_web::AppError;
use rust_toon_system_api::{
    CurrentUserResponse, LoginRequest, LogoutRequest, RefreshTokenRequest, SystemCapability,
    TokenResponse,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{SystemState, application};

pub fn routes(state: SystemState) -> Router {
    let protected = Router::new()
        .route("/system/auth/me", get(me))
        .route("/system/auth/get-permission-info", get(permission_info))
        .merge(crate::audit::routes())
        .merge(crate::management::routes())
        .route_layer(from_fn_with_state(state.tokens.clone(), authenticate));

    Router::new()
        .route("/system/capabilities", get(capabilities))
        .route("/system/auth/login", post(login))
        .route("/system/tenant/simple-list", get(tenant_simple_list))
        .route("/system/tenant/get-by-website", get(tenant_by_website))
        .route(
            "/system/auth/refresh-token",
            get(refresh_token).post(refresh_token),
        )
        .route("/system/auth/logout", post(logout))
        .merge(protected)
        .with_state(state)
}

async fn capabilities() -> Json<ApiResponse<SystemCapability>> {
    Json(ApiResponse::new(SystemCapability::default()))
}

async fn tenant_simple_list(
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let rows = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object(
                    'id', id,
                    'name', name,
                    'contactUserId', contact_user_id,
                    'contactName', contact_name,
                    'contactMobile', contact_mobile,
                    'status', status,
                    'websites', string_to_array(websites, ','),
                    'packageId', package_id,
                    'expireTime', expire_time,
                    'accountCount', account_count,
                    'createTime', create_time,
                    'updateTime', update_time
                )
         FROM system_tenant
         WHERE deleted = 0 AND status = 0
         ORDER BY id",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list tenants"))?;

    Ok(Json(ApiResponse::new(rows)))
}

async fn tenant_by_website(
    State(state): State<SystemState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let website = params
        .get("website")
        .map(String::as_str)
        .unwrap_or_default();
    let row = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object(
                    'id', id,
                    'name', name,
                    'contactUserId', contact_user_id,
                    'contactName', contact_name,
                    'contactMobile', contact_mobile,
                    'status', status,
                    'websites', string_to_array(websites, ','),
                    'packageId', package_id,
                    'expireTime', expire_time,
                    'accountCount', account_count,
                    'createTime', create_time,
                    'updateTime', update_time
                )
         FROM system_tenant
         WHERE deleted = 0 AND status = 0
           AND ($1 = '' OR $1 = ANY(string_to_array(websites, ',')))
         ORDER BY id
         LIMIT 1",
    )
    .bind(website)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to get tenant"))?;
    Ok(Json(ApiResponse::new(row.unwrap_or_else(|| json!({})))))
}

async fn login(
    State(state): State<SystemState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<ApiResponse<TokenResponse>>, AppError> {
    application::login(&state, request)
        .await
        .map(|response| Json(ApiResponse::new(response)))
        .map_err(|error| match error {
            application::LoginError::InvalidCredentials => {
                AppError::unauthorized("invalid username or password")
            }
            application::LoginError::Disabled => AppError::forbidden("account is disabled"),
            application::LoginError::Locked => AppError::forbidden("account is locked"),
            application::LoginError::Internal => AppError::internal("authentication failed"),
        })
}

#[derive(Debug, Deserialize)]
struct RefreshTokenQuery {
    #[serde(alias = "refreshToken")]
    refresh_token: String,
}

async fn refresh_token(
    State(state): State<SystemState>,
    Query(query): Query<RefreshTokenQuery>,
) -> Result<Json<ApiResponse<TokenResponse>>, AppError> {
    application::refresh(
        &state,
        RefreshTokenRequest {
            refresh_token: query.refresh_token,
        },
    )
    .await
    .map(|response| Json(ApiResponse::new(response)))
    .map_err(refresh_error)
}

async fn logout(
    State(state): State<SystemState>,
    Json(request): Json<LogoutRequest>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    application::logout(&state, request.refresh_token.as_deref())
        .await
        .map(|()| Json(ApiResponse::new(())))
        .map_err(refresh_error)
}

fn refresh_error(error: application::RefreshError) -> AppError {
    match error {
        application::RefreshError::Invalid => AppError::unauthorized("invalid refresh token"),
        application::RefreshError::Disabled => AppError::forbidden("account is disabled"),
        application::RefreshError::Locked => AppError::forbidden("account is locked"),
        application::RefreshError::Internal => AppError::internal("authentication failed"),
    }
}

async fn me(user: CurrentUser) -> Json<ApiResponse<CurrentUserResponse>> {
    let permissions = user.permissions.iter().map(ToString::to_string).collect();
    let data_scope = match user.data_scope {
        DataScope::SelfOnly => "self_only",
        DataScope::Department => "department",
        DataScope::Organization => "organization",
        DataScope::All => "all",
    };

    Json(ApiResponse::new(CurrentUserResponse {
        user_id: user.user_id,
        username: user.username,
        tenant_id: user.tenant_id,
        role_codes: user.role_codes,
        permissions,
        data_scope: data_scope.into(),
    }))
}

#[derive(sqlx::FromRow)]
struct AuthorizedMenuRow {
    id: i64,
    parent_id: i64,
    sort: i32,
    name: String,
    path: Option<String>,
    component: Option<String>,
    component_name: Option<String>,
    active_menu_id: Option<i64>,
    icon: Option<String>,
    visible: bool,
    keep_alive: bool,
    always_show: bool,
}

async fn authorized_menus(
    pool: &sqlx::PgPool,
    user_id: &str,
) -> Result<Vec<AuthorizedMenuRow>, AppError> {
    sqlx::query_as::<_, AuthorizedMenuRow>(
        "SELECT DISTINCT m.id, m.parent_id, m.sort, m.name, m.path, m.component,
                m.component_name, m.active_menu_id, m.icon, m.visible,
                m.keep_alive, m.always_show
         FROM system_users u
         JOIN system_tenant tenant
           ON tenant.id = u.tenant_id AND tenant.deleted = 0 AND tenant.status = 0
         LEFT JOIN system_tenant_package package
           ON package.id = tenant.package_id AND package.deleted = 0 AND package.status = 0
         JOIN system_user_role ur ON ur.user_id = u.id AND ur.deleted = 0
         JOIN system_role r ON r.id = ur.role_id AND r.deleted = 0 AND r.status = 0
         LEFT JOIN system_role_menu rm ON rm.role_id = r.id AND rm.deleted = 0
         JOIN system_menu m ON m.deleted = 0 AND m.status = 0 AND m.type <> 3
             AND (
                 r.code = 'super_admin'
                 OR (
                     -- 租户管理员：菜单直接由套餐推导（单一数据源，不依赖
                     -- role_menu 快照；套餐调整即时生效）。
                     r.code = 'tenant_admin'
                     AND EXISTS (
                         SELECT 1
                         FROM jsonb_array_elements_text(package.menu_ids::jsonb) ta(menu_id)
                         WHERE ta.menu_id::bigint = m.id
                     )
                 )
                 OR (
                     r.code = 'tenant_admin'
                     AND m.visible = false
                     AND m.active_menu_id IS NOT NULL
                     AND EXISTS (
                         SELECT 1
                         FROM jsonb_array_elements_text(package.menu_ids::jsonb) ta(menu_id)
                         WHERE ta.menu_id::bigint = m.active_menu_id
                     )
                 )
                 OR m.id = rm.menu_id
                 OR (
                     m.visible = false
                     AND m.active_menu_id IS NOT NULL
                     AND EXISTS (
                         SELECT 1
                         FROM system_role_menu owner_rm
                         WHERE owner_rm.role_id = r.id
                           AND owner_rm.menu_id = m.active_menu_id
                           AND owner_rm.deleted = 0
                     )
                 )
             )
             AND (
                 tenant.package_id = 0
                 OR EXISTS (
                     SELECT 1
                     FROM jsonb_array_elements_text(package.menu_ids::jsonb) allowed(menu_id)
                     WHERE allowed.menu_id::bigint = m.id
                 )
                 OR (
                     m.visible = false
                     AND m.active_menu_id IS NOT NULL
                     AND EXISTS (
                         SELECT 1
                         FROM jsonb_array_elements_text(package.menu_ids::jsonb) allowed(menu_id)
                         WHERE allowed.menu_id::bigint = m.active_menu_id
                     )
                 )
             )
         WHERE u.identity_uuid = $1::uuid
           AND u.deleted = 0 AND u.status = 0
         ORDER BY m.id",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to load authorized menus"))
}

async fn permission_info(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let profile = sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object(
                    'id', id, 'nickname', nickname, 'avatar', avatar,
                    'deptId', dept_id, 'username', username, 'email', email)
         FROM system_users
         WHERE identity_uuid = $1::uuid AND deleted = 0
         ORDER BY id
         LIMIT 1",
    )
    .bind(&user.user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to load user profile"))?
    .unwrap_or_else(|| {
        json!({
            "id": user.user_id,
            "nickname": user.username,
            "avatar": "",
            "deptId": null,
            "username": user.username,
            "email": ""
        })
    });

    let menus = authorized_menus(&state.pool, &user.user_id).await?;

    let menu_tree = build_menu_tree(&menus, 0);
    let permissions = user
        .permissions
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(Json(ApiResponse::new(json!({
        "user": profile,
        "roles": user.role_codes,
        "permissions": permissions,
        "menus": menu_tree
    }))))
}

fn build_menu_tree(rows: &[AuthorizedMenuRow], parent_id: i64) -> Vec<Value> {
    let mut children = rows
        .iter()
        .filter(|row| row.parent_id == parent_id)
        .map(|row| {
            json!({
                "id": row.id,
                "parentId": row.parent_id,
                "name": row.name,
                "path": row.path,
                "component": row.component,
                "componentName": row.component_name,
                "activeMenuId": row.active_menu_id,
                "icon": row.icon,
                "visible": row.visible,
                "keepAlive": row.keep_alive,
                "alwaysShow": row.always_show,
                "children": build_menu_tree(rows, row.id)
            })
        })
        .collect::<Vec<_>>();
    children.sort_by_key(|node| {
        rows.iter()
            .find(|row| row.id == node["id"].as_i64().unwrap_or_default())
            .map(|row| (row.sort, row.id))
            .unwrap_or_default()
    });
    children
}
