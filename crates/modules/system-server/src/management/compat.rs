use std::collections::HashMap;

use crate::{
    SystemState,
    management::{
        compat_fields::{
            bool_field, csv_list_field, i16_field, i32_field, i64_field, i64_vec_field,
            opt_i16_field, opt_i64_field, opt_str_field, parse_i64_param, parse_i64_value,
            split_i64_ids, str_field, str_field_default, string_list_field,
        },
        shared::{parse_id, require},
    },
};
use axum::{
    Json,
    extract::{Query, State},
};
use chrono::{DateTime, Utc};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::{CurrentUser, seal_secret};
use rust_toon_framework_web::AppError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

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
    #[serde(default)]
    name: Option<String>,
    #[serde(default, rename = "type")]
    resource_type: Option<String>,
    #[serde(default)]
    status: Option<i16>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default, rename = "dictType")]
    dict_type: Option<String>,
}

pub fn routes() -> axum::Router<SystemState> {
    axum::Router::new()
        .route("/system/user/page", axum::routing::get(user_page))
        .route("/system/user/list", axum::routing::get(user_list))
        .route(
            "/system/user/simple-list",
            axum::routing::get(user_simple_list),
        )
        .route("/system/user/get-simple", axum::routing::get(user_get))
        .route(
            "/system/user/list-by-nickname",
            axum::routing::get(user_simple_list),
        )
        .route("/system/user/get", axum::routing::get(user_get))
        .route("/system/user/create", axum::routing::post(user_create))
        .route("/system/user/update", axum::routing::put(user_update))
        .route(
            "/system/user/update-status",
            axum::routing::put(user_update_status),
        )
        .route(
            "/system/user/update-password",
            axum::routing::put(user_update_password),
        )
        .route("/system/user/delete", axum::routing::delete(user_delete))
        .route(
            "/system/user/delete-list",
            axum::routing::delete(user_delete_list),
        )
        .route(
            "/system/user/export-excel",
            axum::routing::get(super::excel::user_export),
        )
        .route(
            "/system/user/get-import-template",
            axum::routing::get(super::excel::user_import_template),
        )
        .route(
            "/system/user/import",
            axum::routing::post(super::excel::user_import),
        )
        .route("/system/role/page", axum::routing::get(role_page))
        .route("/system/role/simple-list", axum::routing::get(role_list))
        .route("/system/role/get", axum::routing::get(role_get))
        .route("/system/role/create", axum::routing::post(role_create))
        .route("/system/role/update", axum::routing::put(role_update))
        .route("/system/role/delete", axum::routing::delete(role_delete))
        .route(
            "/system/role/delete-list",
            axum::routing::delete(role_delete_list),
        )
        .route(
            "/system/role/export-excel",
            axum::routing::get(super::excel::role_export),
        )
        .route(
            "/system/permission/list-user-roles",
            axum::routing::get(permission_user_roles),
        )
        .route(
            "/system/permission/assign-user-role",
            axum::routing::post(permission_assign_user_roles),
        )
        .route(
            "/system/permission/list-role-menus",
            axum::routing::get(permission_role_menus),
        )
        .route(
            "/system/permission/assign-role-menu",
            axum::routing::post(permission_assign_role_menus),
        )
        .route(
            "/system/permission/assign-role-data-scope",
            axum::routing::post(role_update_data_scope),
        )
        .route("/system/menu/simple-list", axum::routing::get(menu_list))
        .route("/system/menu/list", axum::routing::get(menu_list))
        .route("/system/menu/get", axum::routing::get(generic_get_menu))
        .route(
            "/system/menu/create",
            axum::routing::post(generic_create_menu),
        )
        .route(
            "/system/menu/update",
            axum::routing::put(generic_update_menu),
        )
        .route(
            "/system/menu/delete",
            axum::routing::delete(generic_delete_menu),
        )
        .route(
            "/system/menu/delete-list",
            axum::routing::delete(generic_delete_list_menu),
        )
        .route(
            "/system/dept/simple-list",
            axum::routing::get(generic_list_dept),
        )
        .route("/system/dept/list", axum::routing::get(generic_list_dept))
        .route("/system/dept/get", axum::routing::get(generic_get_dept))
        .route(
            "/system/dept/create",
            axum::routing::post(generic_create_dept),
        )
        .route(
            "/system/dept/update",
            axum::routing::put(generic_update_dept),
        )
        .route(
            "/system/dept/delete",
            axum::routing::delete(generic_delete_dept),
        )
        .route(
            "/system/dept/delete-list",
            axum::routing::delete(generic_delete_list_dept),
        )
        .route("/system/post/page", axum::routing::get(generic_page_post))
        .route(
            "/system/post/simple-list",
            axum::routing::get(generic_list_post),
        )
        .route("/system/post/get", axum::routing::get(generic_get_post))
        .route(
            "/system/post/create",
            axum::routing::post(generic_create_post),
        )
        .route(
            "/system/post/update",
            axum::routing::put(generic_update_post),
        )
        .route(
            "/system/post/delete",
            axum::routing::delete(generic_delete_post),
        )
        .route(
            "/system/post/delete-list",
            axum::routing::delete(generic_delete_list_post),
        )
        .route(
            "/system/post/export-excel",
            axum::routing::get(super::excel::post_export),
        )
        .route(
            "/system/dict-type/list-all-simple",
            axum::routing::get(generic_list_dict_type),
        )
        .route(
            "/system/dict-type/page",
            axum::routing::get(generic_page_dict_type),
        )
        .route(
            "/system/dict-type/get",
            axum::routing::get(generic_get_dict_type),
        )
        .route(
            "/system/dict-type/create",
            axum::routing::post(generic_create_dict_type),
        )
        .route(
            "/system/dict-type/update",
            axum::routing::put(generic_update_dict_type),
        )
        .route(
            "/system/dict-type/delete",
            axum::routing::delete(generic_delete_dict_type),
        )
        .route(
            "/system/dict-type/delete-list",
            axum::routing::delete(generic_delete_list_dict_type),
        )
        .route(
            "/system/dict-type/export-excel",
            axum::routing::get(super::excel::dict_type_export),
        )
        .route(
            "/system/dict-data/simple-list",
            axum::routing::get(generic_list_dict_data),
        )
        .route(
            "/system/dict-data/page",
            axum::routing::get(generic_page_dict_data),
        )
        .route(
            "/system/dict-data/get",
            axum::routing::get(generic_get_dict_data),
        )
        .route(
            "/system/dict-data/create",
            axum::routing::post(generic_create_dict_data),
        )
        .route(
            "/system/dict-data/update",
            axum::routing::put(generic_update_dict_data),
        )
        .route(
            "/system/dict-data/delete",
            axum::routing::delete(generic_delete_dict_data),
        )
        .route(
            "/system/dict-data/delete-list",
            axum::routing::delete(generic_delete_list_dict_data),
        )
        .route(
            "/system/dict-data/export-excel",
            axum::routing::get(super::excel::dict_data_export),
        )
        .route(
            "/system/tenant/page",
            axum::routing::get(generic_page_tenant),
        )
        .route("/system/tenant/get", axum::routing::get(generic_get_tenant))
        .route(
            "/system/tenant/create",
            axum::routing::post(generic_create_tenant),
        )
        .route(
            "/system/tenant/update",
            axum::routing::put(generic_update_tenant),
        )
        .route(
            "/system/tenant/delete",
            axum::routing::delete(generic_delete_tenant),
        )
        .route(
            "/system/tenant/delete-list",
            axum::routing::delete(generic_delete_list_tenant),
        )
        .route(
            "/system/tenant/export-excel",
            axum::routing::get(super::excel::tenant_export),
        )
        .route(
            "/system/tenant-package/page",
            axum::routing::get(generic_page_tenant_package),
        )
        .route(
            "/system/tenant-package/get-simple-list",
            axum::routing::get(generic_list_tenant_package),
        )
        .route(
            "/system/tenant-package/get",
            axum::routing::get(generic_get_tenant_package),
        )
        .route(
            "/system/tenant-package/create",
            axum::routing::post(generic_create_tenant_package),
        )
        .route(
            "/system/tenant-package/update",
            axum::routing::put(generic_update_tenant_package),
        )
        .route(
            "/system/tenant-package/delete",
            axum::routing::delete(generic_delete_tenant_package),
        )
        .route(
            "/system/tenant-package/delete-list",
            axum::routing::delete(generic_delete_list_tenant_package),
        )
        .route("/system/area/tree", axum::routing::get(area_tree))
        .route("/system/area/get-by-ip", axum::routing::get(area_by_ip))
        .route(
            "/system/operate-log/page",
            axum::routing::get(operate_log_page),
        )
        .route(
            "/system/operate-log/export-excel",
            axum::routing::get(super::excel::operate_log_export),
        )
        .route("/system/login-log/page", axum::routing::get(login_log_page))
        .route(
            "/system/login-log/export-excel",
            axum::routing::get(super::excel::login_log_export),
        )
        .route(
            "/system/notice/page",
            axum::routing::get(generic_page_notice),
        )
        .route("/system/notice/get", axum::routing::get(generic_get_notice))
        .route(
            "/system/notice/create",
            axum::routing::post(generic_create_notice),
        )
        .route(
            "/system/notice/update",
            axum::routing::put(generic_update_notice),
        )
        .route(
            "/system/notice/delete",
            axum::routing::delete(generic_delete_notice),
        )
        .route(
            "/system/notice/delete-list",
            axum::routing::delete(generic_delete_list_notice),
        )
        .route(
            "/system/notice/push",
            axum::routing::post(super::messaging::notice_push),
        )
        .route("/system/user/profile/get", axum::routing::get(profile_get))
        .route(
            "/system/user/profile/update",
            axum::routing::put(profile_update),
        )
        .route(
            "/system/user/profile/update-password",
            axum::routing::put(profile_password),
        )
        .route(
            "/system/oauth2-client/page",
            axum::routing::get(generic_page_oauth2_client),
        )
        .route(
            "/system/oauth2-client/get",
            axum::routing::get(generic_get_oauth2_client),
        )
        .route(
            "/system/oauth2-client/create",
            axum::routing::post(generic_create_oauth2_client),
        )
        .route(
            "/system/oauth2-client/update",
            axum::routing::put(generic_update_oauth2_client),
        )
        .route(
            "/system/oauth2-client/delete",
            axum::routing::delete(generic_delete_oauth2_client),
        )
        .route(
            "/system/oauth2-client/delete-list",
            axum::routing::delete(generic_delete_list_oauth2_client),
        )
        .route(
            "/system/oauth2-token/page",
            axum::routing::get(oauth2_token_page),
        )
        .route(
            "/system/oauth2-token/delete",
            axum::routing::delete(oauth2_token_delete),
        )
        .route(
            "/system/social-client/page",
            axum::routing::get(generic_page_social_client),
        )
        .route(
            "/system/social-client/get",
            axum::routing::get(generic_get_social_client),
        )
        .route(
            "/system/social-client/create",
            axum::routing::post(generic_create_social_client),
        )
        .route(
            "/system/social-client/update",
            axum::routing::put(generic_update_social_client),
        )
        .route(
            "/system/social-client/delete",
            axum::routing::delete(generic_delete_social_client),
        )
        .route(
            "/system/social-client/delete-list",
            axum::routing::delete(generic_delete_list_social_client),
        )
        .route(
            "/system/social-user/page",
            axum::routing::get(social_user_page),
        )
        .route(
            "/system/social-user/get",
            axum::routing::get(social_user_get),
        )
        .route("/system/social-user/bind", axum::routing::post(ok_bool))
        .route("/system/social-user/unbind", axum::routing::delete(ok_bool))
        .route(
            "/system/social-user/simple-list",
            axum::routing::get(social_user_list),
        )
        .route(
            "/system/social-user/get-bind-list",
            axum::routing::get(social_user_bind_list),
        )
        .route(
            "/system/notify-template/page",
            axum::routing::get(generic_page_notify_template),
        )
        .route(
            "/system/notify-template/simple-list",
            axum::routing::get(generic_list_notify_template),
        )
        .route(
            "/system/notify-template/get",
            axum::routing::get(generic_get_notify_template),
        )
        .route(
            "/system/notify-template/create",
            axum::routing::post(generic_create_notify_template),
        )
        .route(
            "/system/notify-template/update",
            axum::routing::put(generic_update_notify_template),
        )
        .route(
            "/system/notify-template/delete",
            axum::routing::delete(generic_delete_notify_template),
        )
        .route(
            "/system/notify-template/delete-list",
            axum::routing::delete(generic_delete_list_notify_template),
        )
        .route(
            "/system/notify-template/export-excel",
            axum::routing::get(super::excel::notify_template_export),
        )
        .route(
            "/system/notify-template/send-notify",
            axum::routing::post(super::notify::send_notify),
        )
        .route(
            "/system/notify-message/page",
            axum::routing::get(generic_page_notify_message),
        )
        .route(
            "/system/notify-message/my-page",
            axum::routing::get(super::notify::my_page),
        )
        .route(
            "/system/notify-message/my-list",
            axum::routing::get(super::notify::my_list),
        )
        .route(
            "/system/notify-message/get-unread-list",
            axum::routing::get(super::notify::unread_list),
        )
        .route(
            "/system/notify-message/get-unread-count",
            axum::routing::get(super::notify::unread_count),
        )
        .route(
            "/system/notify-message/update-read",
            axum::routing::put(super::notify::update_read),
        )
        .route(
            "/system/notify-message/update-all-read",
            axum::routing::put(super::notify::update_all_read),
        )
        .route(
            "/system/mail-account/page",
            axum::routing::get(generic_page_mail_account),
        )
        .route(
            "/system/mail-account/simple-list",
            axum::routing::get(generic_list_mail_account),
        )
        .route(
            "/system/mail-account/get",
            axum::routing::get(generic_get_mail_account),
        )
        .route(
            "/system/mail-account/create",
            axum::routing::post(generic_create_mail_account),
        )
        .route(
            "/system/mail-account/update",
            axum::routing::put(generic_update_mail_account),
        )
        .route(
            "/system/mail-account/delete",
            axum::routing::delete(generic_delete_mail_account),
        )
        .route(
            "/system/mail-account/delete-list",
            axum::routing::delete(generic_delete_list_mail_account),
        )
        .route(
            "/system/mail-template/page",
            axum::routing::get(generic_page_mail_template),
        )
        .route(
            "/system/mail-template/simple-list",
            axum::routing::get(generic_list_mail_template),
        )
        .route(
            "/system/mail-template/get",
            axum::routing::get(generic_get_mail_template),
        )
        .route(
            "/system/mail-template/create",
            axum::routing::post(generic_create_mail_template),
        )
        .route(
            "/system/mail-template/update",
            axum::routing::put(generic_update_mail_template),
        )
        .route(
            "/system/mail-template/delete",
            axum::routing::delete(generic_delete_mail_template),
        )
        .route(
            "/system/mail-template/delete-list",
            axum::routing::delete(generic_delete_list_mail_template),
        )
        .route(
            "/system/mail-template/send-mail",
            axum::routing::post(super::messaging::mail_template_send),
        )
        .route(
            "/system/mail-log/page",
            axum::routing::get(generic_page_mail_log),
        )
        .route(
            "/system/mail-log/export-excel",
            axum::routing::get(super::excel::mail_log_export),
        )
        .route(
            "/system/sms-channel/page",
            axum::routing::get(generic_page_sms_channel),
        )
        .route(
            "/system/sms-channel/simple-list",
            axum::routing::get(generic_list_sms_channel),
        )
        .route(
            "/system/sms-channel/get",
            axum::routing::get(generic_get_sms_channel),
        )
        .route(
            "/system/sms-channel/create",
            axum::routing::post(generic_create_sms_channel),
        )
        .route(
            "/system/sms-channel/update",
            axum::routing::put(generic_update_sms_channel),
        )
        .route(
            "/system/sms-channel/delete",
            axum::routing::delete(generic_delete_sms_channel),
        )
        .route(
            "/system/sms-channel/delete-list",
            axum::routing::delete(generic_delete_list_sms_channel),
        )
        .route(
            "/system/sms-channel/export-excel",
            axum::routing::get(super::excel::sms_channel_export),
        )
        .route(
            "/system/sms-template/page",
            axum::routing::get(generic_page_sms_template),
        )
        .route(
            "/system/sms-template/simple-list",
            axum::routing::get(generic_list_sms_template),
        )
        .route(
            "/system/sms-template/get",
            axum::routing::get(generic_get_sms_template),
        )
        .route(
            "/system/sms-template/create",
            axum::routing::post(generic_create_sms_template),
        )
        .route(
            "/system/sms-template/update",
            axum::routing::put(generic_update_sms_template),
        )
        .route(
            "/system/sms-template/delete",
            axum::routing::delete(generic_delete_sms_template),
        )
        .route(
            "/system/sms-template/delete-list",
            axum::routing::delete(generic_delete_list_sms_template),
        )
        .route(
            "/system/sms-template/export-excel",
            axum::routing::get(super::excel::sms_template_export),
        )
        .route(
            "/system/sms-template/send-sms",
            axum::routing::post(super::messaging::sms_template_send),
        )
        .route(
            "/system/sms-log/page",
            axum::routing::get(generic_page_sms_log),
        )
        .route(
            "/system/sms-log/export-excel",
            axum::routing::get(super::excel::sms_log_export),
        )
        .route(
            "/system/oauth2/authorize",
            axum::routing::get(oauth2_authorize_get).post(oauth2_authorize_post),
        )
}

macro_rules! generic_page_handler {
    ($kind:literal, $handler:ident) => {
        async fn $handler(
            user: CurrentUser,
            State(state): State<SystemState>,
            Query(params): Query<QueryParams>,
        ) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
            require_resource(&user, $kind, "query")?;
            generic_page(&state.pool, $kind, params).await
        }
    };
}

macro_rules! generic_list_handler {
    ($kind:literal, $handler:ident) => {
        async fn $handler(
            user: CurrentUser,
            State(state): State<SystemState>,
            Query(params): Query<QueryParams>,
        ) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
            require_resource(&user, $kind, "query")?;
            generic_list(&state.pool, $kind, params).await
        }
    };
}

macro_rules! generic_get_handler {
    ($kind:literal, $handler:ident) => {
        async fn $handler(
            user: CurrentUser,
            State(state): State<SystemState>,
            Query(params): Query<HashMap<String, String>>,
        ) -> Result<Json<ApiResponse<Value>>, AppError> {
            require_resource(&user, $kind, "query")?;
            generic_get(&state.pool, $kind, params).await
        }
    };
}

macro_rules! generic_create_handler {
    ($kind:literal, $handler:ident) => {
        async fn $handler(
            user: CurrentUser,
            State(state): State<SystemState>,
            Json(payload): Json<Value>,
        ) -> Result<Json<ApiResponse<String>>, AppError> {
            require_resource(&user, $kind, "create")?;
            let result = generic_create(&state.pool, $kind, payload).await;
            if result.is_ok() && invalidates_authorization($kind) {
                crate::cache::invalidate_all_current_users(&state).await;
            }
            result
        }
    };
}

macro_rules! generic_update_handler {
    ($kind:literal, $handler:ident) => {
        async fn $handler(
            user: CurrentUser,
            State(state): State<SystemState>,
            Json(payload): Json<Value>,
        ) -> Result<Json<ApiResponse<()>>, AppError> {
            require_resource(&user, $kind, "update")?;
            let result = generic_update(&state.pool, $kind, payload).await;
            if result.is_ok() && invalidates_authorization($kind) {
                crate::cache::invalidate_all_current_users(&state).await;
            }
            result
        }
    };
}

macro_rules! generic_delete_handler {
    ($kind:literal, $handler:ident) => {
        async fn $handler(
            user: CurrentUser,
            State(state): State<SystemState>,
            Query(params): Query<HashMap<String, String>>,
        ) -> Result<Json<ApiResponse<()>>, AppError> {
            require_resource(&user, $kind, "delete")?;
            let result = generic_delete(&state.pool, $kind, params).await;
            if result.is_ok() && invalidates_authorization($kind) {
                crate::cache::invalidate_all_current_users(&state).await;
            }
            result
        }
    };
}

macro_rules! generic_delete_list_handler {
    ($kind:literal, $handler:ident) => {
        async fn $handler(
            user: CurrentUser,
            State(state): State<SystemState>,
            Query(params): Query<HashMap<String, String>>,
        ) -> Result<Json<ApiResponse<()>>, AppError> {
            require_resource(&user, $kind, "delete")?;
            let result = generic_delete_list(&state.pool, $kind, params).await;
            if result.is_ok() && invalidates_authorization($kind) {
                crate::cache::invalidate_all_current_users(&state).await;
            }
            result
        }
    };
}

macro_rules! generic_resource_handlers {
    ($kind:literal, $page:ident, $list:ident, $get:ident, $create:ident, $update:ident, $delete:ident, $delete_list:ident) => {
        generic_page_handler!($kind, $page);
        generic_list_handler!($kind, $list);
        generic_get_handler!($kind, $get);
        generic_create_handler!($kind, $create);
        generic_update_handler!($kind, $update);
        generic_delete_handler!($kind, $delete);
        generic_delete_list_handler!($kind, $delete_list);
    };
}

generic_list_handler!("dept", generic_list_dept);
generic_get_handler!("menu", generic_get_menu);
generic_create_handler!("menu", generic_create_menu);
generic_update_handler!("menu", generic_update_menu);
generic_delete_handler!("menu", generic_delete_menu);
generic_delete_list_handler!("menu", generic_delete_list_menu);
generic_get_handler!("dept", generic_get_dept);
generic_create_handler!("dept", generic_create_dept);
generic_update_handler!("dept", generic_update_dept);
generic_delete_handler!("dept", generic_delete_dept);
generic_delete_list_handler!("dept", generic_delete_list_dept);

generic_resource_handlers!(
    "post",
    generic_page_post,
    generic_list_post,
    generic_get_post,
    generic_create_post,
    generic_update_post,
    generic_delete_post,
    generic_delete_list_post
);
generic_resource_handlers!(
    "dict_type",
    generic_page_dict_type,
    generic_list_dict_type,
    generic_get_dict_type,
    generic_create_dict_type,
    generic_update_dict_type,
    generic_delete_dict_type,
    generic_delete_list_dict_type
);
generic_resource_handlers!(
    "dict_data",
    generic_page_dict_data,
    generic_list_dict_data,
    generic_get_dict_data,
    generic_create_dict_data,
    generic_update_dict_data,
    generic_delete_dict_data,
    generic_delete_list_dict_data
);
generic_resource_handlers!(
    "tenant_package",
    generic_page_tenant_package,
    generic_list_tenant_package,
    generic_get_tenant_package,
    generic_create_tenant_package,
    generic_update_tenant_package,
    generic_delete_tenant_package,
    generic_delete_list_tenant_package
);
generic_resource_handlers!(
    "notify_template",
    generic_page_notify_template,
    generic_list_notify_template,
    generic_get_notify_template,
    generic_create_notify_template,
    generic_update_notify_template,
    generic_delete_notify_template,
    generic_delete_list_notify_template
);
generic_resource_handlers!(
    "mail_account",
    generic_page_mail_account,
    generic_list_mail_account,
    generic_get_mail_account,
    generic_create_mail_account,
    generic_update_mail_account,
    generic_delete_mail_account,
    generic_delete_list_mail_account
);
generic_resource_handlers!(
    "mail_template",
    generic_page_mail_template,
    generic_list_mail_template,
    generic_get_mail_template,
    generic_create_mail_template,
    generic_update_mail_template,
    generic_delete_mail_template,
    generic_delete_list_mail_template
);
generic_resource_handlers!(
    "sms_channel",
    generic_page_sms_channel,
    generic_list_sms_channel,
    generic_get_sms_channel,
    generic_create_sms_channel,
    generic_update_sms_channel,
    generic_delete_sms_channel,
    generic_delete_list_sms_channel
);
generic_resource_handlers!(
    "sms_template",
    generic_page_sms_template,
    generic_list_sms_template,
    generic_get_sms_template,
    generic_create_sms_template,
    generic_update_sms_template,
    generic_delete_sms_template,
    generic_delete_list_sms_template
);

generic_page_handler!("tenant", generic_page_tenant);
generic_get_handler!("tenant", generic_get_tenant);
generic_create_handler!("tenant", generic_create_tenant);
generic_update_handler!("tenant", generic_update_tenant);
generic_delete_handler!("tenant", generic_delete_tenant);
generic_delete_list_handler!("tenant", generic_delete_list_tenant);

generic_page_handler!("notice", generic_page_notice);
generic_get_handler!("notice", generic_get_notice);
generic_create_handler!("notice", generic_create_notice);
generic_update_handler!("notice", generic_update_notice);
generic_delete_handler!("notice", generic_delete_notice);
generic_delete_list_handler!("notice", generic_delete_list_notice);

generic_page_handler!("oauth2_client", generic_page_oauth2_client);
generic_get_handler!("oauth2_client", generic_get_oauth2_client);
generic_create_handler!("oauth2_client", generic_create_oauth2_client);
generic_update_handler!("oauth2_client", generic_update_oauth2_client);
generic_delete_handler!("oauth2_client", generic_delete_oauth2_client);
generic_delete_list_handler!("oauth2_client", generic_delete_list_oauth2_client);

generic_page_handler!("social_client", generic_page_social_client);
generic_get_handler!("social_client", generic_get_social_client);
generic_create_handler!("social_client", generic_create_social_client);
generic_update_handler!("social_client", generic_update_social_client);
generic_delete_handler!("social_client", generic_delete_social_client);
generic_delete_list_handler!("social_client", generic_delete_list_social_client);

generic_page_handler!("notify_message", generic_page_notify_message);
generic_page_handler!("mail_log", generic_page_mail_log);
generic_page_handler!("sms_log", generic_page_sms_log);

fn require_resource(user: &CurrentUser, kind: &str, action: &str) -> Result<(), AppError> {
    let resource = match kind {
        "dict_type" | "dict_data" => "dict",
        "tenant_package" => "tenant-package",
        "oauth2_client" => "oauth2-client",
        "oauth2_access_token" => "oauth2-token",
        "social_client" => "social-client",
        "social_user" | "social_user_bind" => "social-user",
        "notify_template" => "notify-template",
        "notify_message" => "notify-message",
        "mail_account" => "mail-account",
        "mail_template" => "mail-template",
        "mail_log" => "mail-log",
        "sms_channel" => "sms-channel",
        "sms_template" => "sms-template",
        "sms_log" => "sms-log",
        other => other,
    };
    require(user, &format!("system:{resource}:{action}"))
}

fn invalidates_authorization(kind: &str) -> bool {
    matches!(kind, "menu" | "tenant_package")
}

fn current_tenant_id(user: &CurrentUser) -> Result<i64, AppError> {
    user.tenant_id
        .as_deref()
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| AppError::forbidden("tenant is required"))
}

async fn uuid_for_yudao_user_id(pool: &PgPool, user_id: i64) -> Result<Uuid, AppError> {
    sqlx::query_scalar("SELECT md5('yudao-user:' || $1::text)::uuid")
        .bind(user_id)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to resolve user cache key"))
}

async fn generic_page(
    pool: &PgPool,
    kind: &str,
    params: QueryParams,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    if is_core_kind(kind) {
        return core_page(pool, kind, params).await;
    }
    if let Some(spec) = yudao_spec(kind) {
        return yudao_page(pool, spec, params).await;
    }
    Err(AppError::bad_request("unsupported record kind"))
}

async fn generic_list(
    pool: &PgPool,
    kind: &str,
    _params: QueryParams,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    if is_core_kind(kind) {
        return core_list(pool, kind).await;
    }
    if let Some(spec) = yudao_spec(kind) {
        return yudao_list(pool, spec).await;
    }
    Err(AppError::bad_request("unsupported record kind"))
}

async fn generic_get(
    pool: &PgPool,
    kind: &str,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    if is_core_kind(kind) {
        return core_get(pool, kind, params).await;
    }
    if let Some(spec) = yudao_spec(kind) {
        return yudao_get(pool, spec, params).await;
    }
    Err(AppError::bad_request("unsupported record kind"))
}

async fn generic_create(
    pool: &PgPool,
    kind: &str,
    payload: Value,
) -> Result<Json<ApiResponse<String>>, AppError> {
    if is_core_kind(kind) {
        return core_create(pool, kind, payload).await;
    }
    if let Some(spec) = yudao_spec(kind) {
        return yudao_create(pool, spec, payload).await;
    }
    Err(AppError::bad_request("unsupported record kind"))
}

async fn generic_update(
    pool: &PgPool,
    kind: &str,
    payload: Value,
) -> Result<Json<ApiResponse<()>>, AppError> {
    if is_core_kind(kind) {
        return core_update(pool, kind, payload).await;
    }
    if let Some(spec) = yudao_spec(kind) {
        return yudao_update(pool, spec, payload).await;
    }
    Err(AppError::bad_request("unsupported record kind"))
}

async fn generic_delete(
    pool: &PgPool,
    kind: &str,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    if is_core_kind(kind) {
        return core_delete(pool, kind, params).await;
    }
    if let Some(spec) = yudao_spec(kind) {
        return yudao_delete(pool, spec, params).await;
    }
    Err(AppError::bad_request("unsupported record kind"))
}

async fn generic_delete_list(
    pool: &PgPool,
    kind: &str,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    if is_core_kind(kind) {
        return core_delete_list(pool, kind, params).await;
    }
    if let Some(spec) = yudao_spec(kind) {
        return yudao_delete_list(pool, spec, params).await;
    }
    Err(AppError::bad_request("unsupported record kind"))
}

#[derive(Clone, Copy)]
struct YudaoSpec {
    table: &'static str,
    seq: &'static str,
}

fn yudao_spec(kind: &str) -> Option<YudaoSpec> {
    let table = match kind {
        "notice" => "system_notice",
        "oauth2_client" => "system_oauth2_client",
        "social_client" => "system_social_client",
        "notify_template" => "system_notify_template",
        "notify_message" => "system_notify_message",
        "mail_account" => "system_mail_account",
        "mail_template" => "system_mail_template",
        "mail_log" => "system_mail_log",
        "sms_channel" => "system_sms_channel",
        "sms_template" => "system_sms_template",
        "sms_log" => "system_sms_log",
        _ => return None,
    };
    Some(YudaoSpec {
        table,
        seq: match table {
            "system_notice" => "system_notice_seq",
            "system_oauth2_client" => "system_oauth2_client_seq",
            "system_social_client" => "system_social_client_seq",
            "system_notify_template" => "system_notify_template_seq",
            "system_notify_message" => "system_notify_message_seq",
            "system_mail_account" => "system_mail_account_seq",
            "system_mail_template" => "system_mail_template_seq",
            "system_mail_log" => "system_mail_log_seq",
            "system_sms_channel" => "system_sms_channel_seq",
            "system_sms_template" => "system_sms_template_seq",
            "system_sms_log" => "system_sms_log_seq",
            _ => unreachable!(),
        },
    })
}

async fn yudao_page(
    pool: &PgPool,
    spec: YudaoSpec,
    params: QueryParams,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    let page_no = params.page_no.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(10).clamp(1, 200);
    let offset = (page_no - 1) * page_size;
    let total_sql = format!("SELECT count(*) FROM {} WHERE deleted = 0", spec.table);
    let total = sqlx::query_scalar::<_, i64>(&total_sql)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to count records"))?;
    let list_sql = format!(
        "SELECT to_jsonb(t) FROM {} t WHERE deleted = 0 ORDER BY id DESC LIMIT $1 OFFSET $2",
        spec.table
    );
    let rows = sqlx::query_scalar::<_, Value>(&list_sql)
        .bind(page_size)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?;
    Ok(Json(ApiResponse::new(Page {
        list: rows.into_iter().map(yudao_value).collect(),
        total,
    })))
}

async fn yudao_list(
    pool: &PgPool,
    spec: YudaoSpec,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let sql = format!(
        "SELECT to_jsonb(t) FROM {} t WHERE deleted = 0 ORDER BY id DESC",
        spec.table
    );
    let rows = sqlx::query_scalar::<_, Value>(&sql)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?;
    Ok(Json(ApiResponse::new(
        rows.into_iter().map(yudao_value).collect(),
    )))
}

async fn yudao_get(
    pool: &PgPool,
    spec: YudaoSpec,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let id = parse_i64_param(&params, "id")?;
    let sql = format!(
        "SELECT to_jsonb(t) FROM {} t WHERE id = $1 AND deleted = 0",
        spec.table
    );
    let row = sqlx::query_scalar::<_, Value>(&sql)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|_| AppError::internal("failed to get record"))?
        .ok_or_else(|| AppError::not_found("record not found"))?;
    Ok(Json(ApiResponse::new(yudao_value(row))))
}

async fn yudao_create(
    pool: &PgPool,
    spec: YudaoSpec,
    payload: Value,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let db_payload = seal_sensitive_payload(spec.table, camel_payload_to_snake(payload), false)?;
    let columns = writable_columns(pool, spec.table, &db_payload, false).await?;
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

async fn yudao_update(
    pool: &PgPool,
    spec: YudaoSpec,
    payload: Value,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = parse_i64_value(&payload["id"])?;
    let db_payload = seal_sensitive_payload(spec.table, camel_payload_to_snake(payload), true)?;
    let columns = writable_columns(pool, spec.table, &db_payload, true).await?;
    if columns.is_empty() {
        return Ok(Json(ApiResponse::new(())));
    }
    let set_sql = columns
        .iter()
        .map(|column| format!("{column}=r.{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "UPDATE {} t SET {}, update_time = now() FROM jsonb_populate_record(NULL::{}, $2::jsonb) AS r WHERE t.id = $1 AND t.deleted = 0",
        spec.table, set_sql, spec.table
    );
    let result = sqlx::query(&sql)
        .bind(id)
        .bind(db_payload)
        .execute(pool)
        .await
        .map_err(|_| AppError::internal("failed to update record"))?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("record not found"));
    }
    Ok(Json(ApiResponse::new(())))
}

async fn yudao_delete(
    pool: &PgPool,
    spec: YudaoSpec,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = parse_i64_param(&params, "id")?;
    let sql = format!(
        "UPDATE {} SET deleted = 1, update_time = now() WHERE id = $1",
        spec.table
    );
    sqlx::query(&sql)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|_| AppError::internal("failed to delete record"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn yudao_delete_list(
    pool: &PgPool,
    spec: YudaoSpec,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    for id in split_i64_ids(&params) {
        let sql = format!(
            "UPDATE {} SET deleted = 1, update_time = now() WHERE id = $1",
            spec.table
        );
        sqlx::query(&sql)
            .bind(id)
            .execute(pool)
            .await
            .map_err(|_| AppError::internal("failed to delete record"))?;
    }
    Ok(Json(ApiResponse::new(())))
}

async fn writable_columns(
    pool: &PgPool,
    table: &str,
    payload: &Value,
    include_id: bool,
) -> Result<Vec<String>, AppError> {
    let Some(object) = payload.as_object() else {
        return Ok(vec![]);
    };
    let columns = sqlx::query_scalar::<_, String>(
        "SELECT column_name FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1",
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
                    "creator" | "create_time" | "updater" | "update_time" | "deleted"
                )
                && columns.iter().any(|column| column == *key)
        })
        .cloned()
        .collect())
}

fn yudao_value(value: Value) -> Value {
    let Value::Object(object) = value else {
        return value;
    };
    let mut mapped = serde_json::Map::new();
    for (key, value) in object {
        if key == "deleted" {
            continue;
        }
        let value = if is_sensitive_column(&key) {
            mask_secret_value(value)
        } else {
            parse_jsonish_value(value)
        };
        mapped.insert(snake_to_camel(&key), value);
    }
    Value::Object(mapped)
}

fn camel_payload_to_snake(value: Value) -> Value {
    let Value::Object(object) = value else {
        return json!({});
    };
    let mut mapped = serde_json::Map::new();
    for (key, value) in object {
        if matches!(key.as_str(), "createTime" | "updateTime") {
            continue;
        }
        mapped.insert(camel_to_snake(&key), value);
    }
    Value::Object(mapped)
}

fn seal_sensitive_payload(table: &str, value: Value, skip_masked: bool) -> Result<Value, AppError> {
    let Value::Object(mut object) = value else {
        return Ok(json!({}));
    };
    let fields: &[&str] = match table {
        "system_mail_account" => &["password"],
        "system_sms_channel" => &["api_key", "api_secret"],
        "system_oauth2_client" => &["secret"],
        "system_social_client" => &["client_secret"],
        _ => &[],
    };
    for field in fields {
        let Some(current) = object.get(*field).and_then(Value::as_str) else {
            continue;
        };
        if current.is_empty() || (skip_masked && current == "******") {
            object.remove(*field);
            continue;
        }
        let sealed = seal_secret(current).map_err(|error| AppError::internal(error.to_string()))?;
        object.insert((*field).to_owned(), Value::String(sealed));
    }
    Ok(Value::Object(object))
}

fn is_sensitive_column(column: &str) -> bool {
    matches!(
        column,
        "password" | "secret" | "client_secret" | "api_key" | "api_secret"
    )
}

fn mask_secret_value(value: Value) -> Value {
    match value {
        Value::Null => Value::String(String::new()),
        Value::String(value) if value.is_empty() => Value::String(String::new()),
        _ => Value::String("******".to_owned()),
    }
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

fn parse_jsonish_value(value: Value) -> Value {
    let Value::String(text) = &value else {
        return value;
    };
    if !(text.starts_with('{') || text.starts_with('[')) {
        return value;
    }
    serde_json::from_str(text).unwrap_or(value)
}

fn is_core_kind(kind: &str) -> bool {
    matches!(
        kind,
        "menu" | "dept" | "post" | "dict_type" | "dict_data" | "tenant" | "tenant_package"
    )
}

async fn core_page(
    pool: &PgPool,
    kind: &str,
    params: QueryParams,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    let page_no = params.page_no.unwrap_or(1).max(1);
    let page_size = params.page_size.unwrap_or(10).clamp(1, 200);
    let offset = (page_no - 1) * page_size;
    if kind == "dict_type" {
        let name = params.name.unwrap_or_default();
        let resource_type = params.resource_type.unwrap_or_default();
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM system_dict_type
             WHERE deleted=0
               AND ($1='' OR name ILIKE '%' || $1 || '%')
               AND ($2='' OR type ILIKE '%' || $2 || '%')
               AND ($3::smallint IS NULL OR status=$3)",
        )
        .bind(&name)
        .bind(&resource_type)
        .bind(params.status)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to count dictionary types"))?;
        let rows = sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_build_object('id', id, 'name', name, 'type', type,
                    'status', status, 'remark', remark, 'createTime', create_time)
             FROM system_dict_type
             WHERE deleted=0
               AND ($1='' OR name ILIKE '%' || $1 || '%')
               AND ($2='' OR type ILIKE '%' || $2 || '%')
               AND ($3::smallint IS NULL OR status=$3)
             ORDER BY id DESC LIMIT $4 OFFSET $5",
        )
        .bind(name)
        .bind(resource_type)
        .bind(params.status)
        .bind(page_size)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list dictionary types"))?;
        return Ok(Json(ApiResponse::new(Page { list: rows, total })));
    }
    if kind == "dict_data" {
        let label = params.label.unwrap_or_default();
        let value = params.value.unwrap_or_default();
        let dict_type = params.dict_type.unwrap_or_default();
        let total = sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM system_dict_data
             WHERE deleted=0
               AND ($1='' OR label ILIKE '%' || $1 || '%')
               AND ($2='' OR value ILIKE '%' || $2 || '%')
               AND ($3='' OR dict_type=$3)
               AND ($4::smallint IS NULL OR status=$4)",
        )
        .bind(&label)
        .bind(&value)
        .bind(&dict_type)
        .bind(params.status)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to count dictionary data"))?;
        let rows = sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_build_object('id', id, 'sort', sort, 'label', label,
                    'value', value, 'dictType', dict_type, 'status', status,
                    'colorType', color_type, 'cssClass', css_class,
                    'remark', remark, 'createTime', create_time)
             FROM system_dict_data
             WHERE deleted=0
               AND ($1='' OR label ILIKE '%' || $1 || '%')
               AND ($2='' OR value ILIKE '%' || $2 || '%')
               AND ($3='' OR dict_type=$3)
               AND ($4::smallint IS NULL OR status=$4)
             ORDER BY sort, id LIMIT $5 OFFSET $6",
        )
        .bind(label)
        .bind(value)
        .bind(dict_type)
        .bind(params.status)
        .bind(page_size)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list dictionary data"))?;
        return Ok(Json(ApiResponse::new(Page { list: rows, total })));
    }
    let total = sqlx::query_scalar::<_, i64>(core_count_sql(kind)?)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to count records"))?;
    let select_sql = core_select_sql(kind, true)?;
    let rows = sqlx::query_scalar::<_, Value>(&select_sql)
        .bind(page_size)
        .bind(offset)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?;
    Ok(Json(ApiResponse::new(Page { list: rows, total })))
}

async fn core_list(pool: &PgPool, kind: &str) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    let select_sql = core_select_sql(kind, false)?;
    let rows = sqlx::query_scalar::<_, Value>(&select_sql)
        .fetch_all(pool)
        .await
        .map_err(|_| AppError::internal("failed to list records"))?;
    Ok(Json(ApiResponse::new(rows)))
}

async fn core_get(
    pool: &PgPool,
    kind: &str,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let id = parse_i64_param(&params, "id")?;
    let row = sqlx::query_scalar::<_, Value>(core_get_sql(kind)?)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|_| AppError::internal("failed to get record"))?
        .ok_or_else(|| AppError::not_found("record not found"))?;
    Ok(Json(ApiResponse::new(row)))
}

async fn core_create(
    pool: &PgPool,
    kind: &str,
    payload: Value,
) -> Result<Json<ApiResponse<String>>, AppError> {
    let id = match kind {
        "menu" => sqlx::query_scalar::<_, i64>(
            "INSERT INTO system_menu (id, name, permission, type, sort, parent_id, path, icon, component, component_name, active_menu_id, status, visible, keep_alive, always_show)
             VALUES (nextval('system_menu_seq'),$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING id",
        )
        .bind(str_field(&payload, "name"))
        .bind(str_field_default(&payload, "permission", ""))
        .bind(i16_field(&payload, "type", 2))
        .bind(i32_field(&payload, "sort", 0))
        .bind(i64_field(&payload, "parentId", 0))
        .bind(str_field_default(&payload, "path", ""))
        .bind(str_field_default(&payload, "icon", "#"))
        .bind(opt_str_field(&payload, "component"))
        .bind(opt_str_field(&payload, "componentName"))
        .bind(opt_i64_field(&payload, "activeMenuId"))
        .bind(i16_field(&payload, "status", 0))
        .bind(bool_field(&payload, "visible", true))
        .bind(bool_field(&payload, "keepAlive", true))
        .bind(bool_field(&payload, "alwaysShow", true))
        .fetch_one(pool)
        .await,
        "dept" => sqlx::query_scalar::<_, i64>(
            "INSERT INTO system_dept (id, name, parent_id, sort, leader_user_id, phone, email, status)
             VALUES (nextval('system_dept_seq'),$1,$2,$3,$4,$5,$6,$7) RETURNING id",
        )
        .bind(str_field(&payload, "name"))
        .bind(i64_field(&payload, "parentId", 0))
        .bind(i32_field(&payload, "sort", 0))
        .bind(opt_i64_field(&payload, "leaderUserId"))
        .bind(opt_str_field(&payload, "phone"))
        .bind(opt_str_field(&payload, "email"))
        .bind(i16_field(&payload, "status", 0))
        .fetch_one(pool)
        .await,
        "post" => sqlx::query_scalar::<_, i64>(
            "INSERT INTO system_post (id, code, name, sort, status, remark)
             VALUES (nextval('system_post_seq'),$1,$2,$3,$4,$5) RETURNING id",
        )
        .bind(str_field(&payload, "code"))
        .bind(str_field(&payload, "name"))
        .bind(i32_field(&payload, "sort", 0))
        .bind(i16_field(&payload, "status", 0))
        .bind(opt_str_field(&payload, "remark"))
        .fetch_one(pool)
        .await,
        "dict_type" => sqlx::query_scalar::<_, i64>(
            "INSERT INTO system_dict_type (id, name, type, status, remark)
             VALUES (nextval('system_dict_type_seq'),$1,$2,$3,$4) RETURNING id",
        )
        .bind(str_field(&payload, "name"))
        .bind(str_field(&payload, "type"))
        .bind(i16_field(&payload, "status", 0))
        .bind(opt_str_field(&payload, "remark"))
        .fetch_one(pool)
        .await,
        "dict_data" => sqlx::query_scalar::<_, i64>(
            "INSERT INTO system_dict_data (id, sort, label, value, dict_type, status, color_type, css_class, remark)
             VALUES (nextval('system_dict_data_seq'),$1,$2,$3,$4,$5,$6,$7,$8) RETURNING id",
        )
        .bind(i32_field(&payload, "sort", 0))
        .bind(str_field(&payload, "label"))
        .bind(str_field(&payload, "value"))
        .bind(str_field(&payload, "dictType"))
        .bind(i16_field(&payload, "status", 0))
        .bind(str_field_default(&payload, "colorType", ""))
        .bind(str_field_default(&payload, "cssClass", ""))
        .bind(opt_str_field(&payload, "remark"))
        .fetch_one(pool)
        .await,
        "tenant_package" => sqlx::query_scalar::<_, i64>(
            "INSERT INTO system_tenant_package (id, name, status, remark, menu_ids)
             VALUES (nextval('system_tenant_package_seq'),$1,$2,$3,$4) RETURNING id",
        )
        .bind(str_field(&payload, "name"))
        .bind(i16_field(&payload, "status", 0))
        .bind(str_field_default(&payload, "remark", ""))
        .bind(payload.get("menuIds").cloned().unwrap_or_else(|| json!([])).to_string())
        .fetch_one(pool)
        .await,
        "tenant" => sqlx::query_scalar::<_, i64>(
            "INSERT INTO system_tenant (id, name, contact_name, contact_mobile, status, websites, package_id, expire_time, account_count)
             VALUES (nextval('system_tenant_seq'),$1,$2,$3,$4,$5,$6,$7::timestamp,$8) RETURNING id",
        )
        .bind(str_field(&payload, "name"))
        .bind(str_field(&payload, "contactName"))
        .bind(opt_str_field(&payload, "contactMobile"))
        .bind(i16_field(&payload, "status", 0))
        .bind(csv_list_field(&payload, "websites"))
        .bind(i64_field(&payload, "packageId", 0))
        .bind(str_field_default(&payload, "expireTime", "2099-12-31 23:59:59"))
        .bind(i32_field(&payload, "accountCount", 100))
        .fetch_one(pool)
        .await,
        _ => return Err(AppError::bad_request("unsupported record kind")),
    }
    .map_err(|_| AppError::internal("failed to create record"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn core_update(
    pool: &PgPool,
    kind: &str,
    payload: Value,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = parse_i64_value(&payload["id"])?;
    let result = match kind {
        "menu" => sqlx::query(
            "UPDATE system_menu SET name=$2, permission=$3, type=$4, sort=$5, parent_id=$6, path=$7, icon=$8, component=$9, component_name=$10, active_menu_id=$11, status=$12, visible=$13, keep_alive=$14, always_show=$15, update_time=now() WHERE id=$1 AND deleted=0",
        )
        .bind(id)
        .bind(str_field(&payload, "name"))
        .bind(str_field_default(&payload, "permission", ""))
        .bind(i16_field(&payload, "type", 2))
        .bind(i32_field(&payload, "sort", 0))
        .bind(i64_field(&payload, "parentId", 0))
        .bind(str_field_default(&payload, "path", ""))
        .bind(str_field_default(&payload, "icon", "#"))
        .bind(opt_str_field(&payload, "component"))
        .bind(opt_str_field(&payload, "componentName"))
        .bind(opt_i64_field(&payload, "activeMenuId"))
        .bind(i16_field(&payload, "status", 0))
        .bind(bool_field(&payload, "visible", true))
        .bind(bool_field(&payload, "keepAlive", true))
        .bind(bool_field(&payload, "alwaysShow", true))
        .execute(pool)
        .await,
        "dept" => sqlx::query("UPDATE system_dept SET name=$2, parent_id=$3, sort=$4, leader_user_id=$5, phone=$6, email=$7, status=$8, update_time=now() WHERE id=$1 AND deleted=0")
            .bind(id).bind(str_field(&payload, "name")).bind(i64_field(&payload, "parentId", 0)).bind(i32_field(&payload, "sort", 0)).bind(opt_i64_field(&payload, "leaderUserId")).bind(opt_str_field(&payload, "phone")).bind(opt_str_field(&payload, "email")).bind(i16_field(&payload, "status", 0)).execute(pool).await,
        "post" => sqlx::query("UPDATE system_post SET code=$2, name=$3, sort=$4, status=$5, remark=$6, update_time=now() WHERE id=$1 AND deleted=0")
            .bind(id).bind(str_field(&payload, "code")).bind(str_field(&payload, "name")).bind(i32_field(&payload, "sort", 0)).bind(i16_field(&payload, "status", 0)).bind(opt_str_field(&payload, "remark")).execute(pool).await,
        "dict_type" => sqlx::query("UPDATE system_dict_type SET name=$2, type=$3, status=$4, remark=$5, update_time=now() WHERE id=$1 AND deleted=0")
            .bind(id).bind(str_field(&payload, "name")).bind(str_field(&payload, "type")).bind(i16_field(&payload, "status", 0)).bind(opt_str_field(&payload, "remark")).execute(pool).await,
        "dict_data" => sqlx::query("UPDATE system_dict_data SET sort=$2, label=$3, value=$4, dict_type=$5, status=$6, color_type=$7, css_class=$8, remark=$9, update_time=now() WHERE id=$1 AND deleted=0")
            .bind(id).bind(i32_field(&payload, "sort", 0)).bind(str_field(&payload, "label")).bind(str_field(&payload, "value")).bind(str_field(&payload, "dictType")).bind(i16_field(&payload, "status", 0)).bind(str_field_default(&payload, "colorType", "")).bind(str_field_default(&payload, "cssClass", "")).bind(opt_str_field(&payload, "remark")).execute(pool).await,
        "tenant_package" => sqlx::query("UPDATE system_tenant_package SET name=$2, status=$3, remark=$4, menu_ids=$5, update_time=now() WHERE id=$1 AND deleted=0")
            .bind(id).bind(str_field(&payload, "name")).bind(i16_field(&payload, "status", 0)).bind(str_field_default(&payload, "remark", "")).bind(payload.get("menuIds").cloned().unwrap_or_else(|| json!([])).to_string()).execute(pool).await,
        "tenant" => sqlx::query("UPDATE system_tenant SET name=$2, contact_name=$3, contact_mobile=$4, status=$5, websites=$6, package_id=$7, expire_time=$8::timestamp, account_count=$9, update_time=now() WHERE id=$1 AND deleted=0")
            .bind(id).bind(str_field(&payload, "name")).bind(str_field(&payload, "contactName")).bind(opt_str_field(&payload, "contactMobile")).bind(i16_field(&payload, "status", 0)).bind(csv_list_field(&payload, "websites")).bind(i64_field(&payload, "packageId", 0)).bind(str_field_default(&payload, "expireTime", "2099-12-31 23:59:59")).bind(i32_field(&payload, "accountCount", 100)).execute(pool).await,
        _ => return Err(AppError::bad_request("unsupported record kind")),
    }
    .map_err(|_| AppError::internal("failed to update record"))?;
    if result.rows_affected() == 0 {
        return Err(AppError::not_found("record not found"));
    }
    Ok(Json(ApiResponse::new(())))
}

async fn core_delete(
    pool: &PgPool,
    kind: &str,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = parse_i64_param(&params, "id")?;
    if kind == "dept" {
        let has_children: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM system_dept WHERE parent_id=$1 AND deleted=0)",
        )
        .bind(id)
        .fetch_one(pool)
        .await
        .map_err(|_| AppError::internal("failed to inspect child departments"))?;
        if has_children {
            return Err(AppError::bad_request("请先删除下级部门"));
        }
    }
    sqlx::query(core_delete_sql(kind)?)
        .bind(id)
        .execute(pool)
        .await
        .map_err(|_| AppError::internal("failed to delete record"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn core_delete_list(
    pool: &PgPool,
    kind: &str,
    params: HashMap<String, String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    for id in split_i64_ids(&params) {
        sqlx::query(core_delete_sql(kind)?)
            .bind(id)
            .execute(pool)
            .await
            .map_err(|_| AppError::internal("failed to delete record"))?;
    }
    Ok(Json(ApiResponse::new(())))
}

fn core_count_sql(kind: &str) -> Result<&'static str, AppError> {
    Ok(match kind {
        "menu" => "SELECT count(*) FROM system_menu WHERE deleted = 0",
        "dept" => "SELECT count(*) FROM system_dept WHERE deleted = 0",
        "post" => "SELECT count(*) FROM system_post WHERE deleted = 0",
        "dict_type" => "SELECT count(*) FROM system_dict_type WHERE deleted = 0",
        "dict_data" => "SELECT count(*) FROM system_dict_data WHERE deleted = 0",
        "tenant" => "SELECT count(*) FROM system_tenant WHERE deleted = 0",
        "tenant_package" => "SELECT count(*) FROM system_tenant_package WHERE deleted = 0",
        _ => return Err(AppError::bad_request("unsupported record kind")),
    })
}

fn core_select_sql(kind: &str, paged: bool) -> Result<String, AppError> {
    let suffix = if paged { " LIMIT $1 OFFSET $2" } else { "" };
    let sql = match kind {
        "menu" => format!(
            "SELECT jsonb_build_object('id', menu.id, 'name', menu.name, 'permission', menu.permission, 'type', menu.type, 'sort', menu.sort, 'parentId', menu.parent_id, 'path', menu.path, 'icon', menu.icon, 'component', menu.component, 'componentName', menu.component_name, 'activeMenuId', menu.active_menu_id, 'activeMenuName', active.name, 'status', menu.status, 'visible', menu.visible, 'keepAlive', menu.keep_alive, 'alwaysShow', menu.always_show, 'createTime', menu.create_time) FROM system_menu menu LEFT JOIN system_menu active ON active.id=menu.active_menu_id AND active.deleted=0 WHERE menu.deleted=0 ORDER BY menu.sort, menu.id{suffix}"
        ),
        "dept" => format!(
            "SELECT jsonb_build_object('id', id, 'name', name, 'parentId', parent_id, 'sort', sort, 'leaderUserId', leader_user_id, 'phone', phone, 'email', email, 'status', status, 'createTime', create_time) FROM system_dept WHERE deleted=0 ORDER BY sort, id{suffix}"
        ),
        "post" => format!(
            "SELECT jsonb_build_object('id', id, 'code', code, 'name', name, 'sort', sort, 'status', status, 'remark', remark, 'createTime', create_time) FROM system_post WHERE deleted=0 ORDER BY sort, id{suffix}"
        ),
        "dict_type" => format!(
            "SELECT jsonb_build_object('id', id, 'name', name, 'type', type, 'status', status, 'remark', remark, 'createTime', create_time) FROM system_dict_type WHERE deleted=0 ORDER BY id DESC{suffix}"
        ),
        "dict_data" => format!(
            "SELECT jsonb_build_object('id', id, 'sort', sort, 'label', label, 'value', value, 'dictType', dict_type, 'status', status, 'colorType', color_type, 'cssClass', css_class, 'remark', remark, 'createTime', create_time) FROM system_dict_data WHERE deleted=0 ORDER BY sort, id{suffix}"
        ),
        "tenant" => format!(
            "SELECT jsonb_build_object('id', id, 'name', name, 'contactUserId', contact_user_id, 'contactName', contact_name, 'contactMobile', contact_mobile, 'status', status, 'websites', string_to_array(websites, ','), 'packageId', package_id, 'expireTime', expire_time, 'accountCount', account_count, 'createTime', create_time) FROM system_tenant WHERE deleted=0 ORDER BY id{suffix}"
        ),
        "tenant_package" => format!(
            "SELECT jsonb_build_object('id', id, 'name', name, 'status', status, 'remark', remark, 'menuIds', menu_ids::jsonb, 'createTime', create_time) FROM system_tenant_package WHERE deleted=0 ORDER BY id{suffix}"
        ),
        _ => return Err(AppError::bad_request("unsupported record kind")),
    };
    Ok(sql)
}

fn core_get_sql(kind: &str) -> Result<&'static str, AppError> {
    Ok(match kind {
        "menu" => {
            "SELECT jsonb_build_object('id', menu.id, 'name', menu.name, 'permission', menu.permission, 'type', menu.type, 'sort', menu.sort, 'parentId', menu.parent_id, 'path', menu.path, 'icon', menu.icon, 'component', menu.component, 'componentName', menu.component_name, 'activeMenuId', menu.active_menu_id, 'activeMenuName', active.name, 'status', menu.status, 'visible', menu.visible, 'keepAlive', menu.keep_alive, 'alwaysShow', menu.always_show, 'createTime', menu.create_time) FROM system_menu menu LEFT JOIN system_menu active ON active.id=menu.active_menu_id AND active.deleted=0 WHERE menu.id=$1 AND menu.deleted=0"
        }
        "dept" => {
            "SELECT jsonb_build_object('id', id, 'name', name, 'parentId', parent_id, 'sort', sort, 'leaderUserId', leader_user_id, 'phone', phone, 'email', email, 'status', status, 'createTime', create_time) FROM system_dept WHERE id=$1 AND deleted=0"
        }
        "post" => {
            "SELECT jsonb_build_object('id', id, 'code', code, 'name', name, 'sort', sort, 'status', status, 'remark', remark, 'createTime', create_time) FROM system_post WHERE id=$1 AND deleted=0"
        }
        "dict_type" => {
            "SELECT jsonb_build_object('id', id, 'name', name, 'type', type, 'status', status, 'remark', remark, 'createTime', create_time) FROM system_dict_type WHERE id=$1 AND deleted=0"
        }
        "dict_data" => {
            "SELECT jsonb_build_object('id', id, 'sort', sort, 'label', label, 'value', value, 'dictType', dict_type, 'status', status, 'colorType', color_type, 'cssClass', css_class, 'remark', remark, 'createTime', create_time) FROM system_dict_data WHERE id=$1 AND deleted=0"
        }
        "tenant" => {
            "SELECT jsonb_build_object('id', id, 'name', name, 'contactUserId', contact_user_id, 'contactName', contact_name, 'contactMobile', contact_mobile, 'status', status, 'websites', string_to_array(websites, ','), 'packageId', package_id, 'expireTime', expire_time, 'accountCount', account_count, 'createTime', create_time) FROM system_tenant WHERE id=$1 AND deleted=0"
        }
        "tenant_package" => {
            "SELECT jsonb_build_object('id', id, 'name', name, 'status', status, 'remark', remark, 'menuIds', menu_ids::jsonb, 'createTime', create_time) FROM system_tenant_package WHERE id=$1 AND deleted=0"
        }
        _ => return Err(AppError::bad_request("unsupported record kind")),
    })
}

fn core_delete_sql(kind: &str) -> Result<&'static str, AppError> {
    Ok(match kind {
        "menu" => "UPDATE system_menu SET deleted=1, update_time=now() WHERE id=$1",
        "dept" => "UPDATE system_dept SET deleted=1, update_time=now() WHERE id=$1",
        "post" => "UPDATE system_post SET deleted=1, update_time=now() WHERE id=$1",
        "dict_type" => "UPDATE system_dict_type SET deleted=1, update_time=now() WHERE id=$1",
        "dict_data" => "UPDATE system_dict_data SET deleted=1, update_time=now() WHERE id=$1",
        "tenant" => "UPDATE system_tenant SET deleted=1, update_time=now() WHERE id=$1",
        "tenant_package" => {
            "UPDATE system_tenant_package SET deleted=1, update_time=now() WHERE id=$1"
        }
        _ => return Err(AppError::bad_request("unsupported record kind")),
    })
}

async fn ok_bool() -> Json<ApiResponse<bool>> {
    Json(ApiResponse::new(true))
}

async fn oauth2_authorize_get(user: CurrentUser) -> Json<ApiResponse<Value>> {
    Json(ApiResponse::new(json!({
        "client": null,
        "scopes": [],
        "user": {
            "id": user.user_id,
            "username": user.username
        }
    })))
}

async fn oauth2_authorize_post() -> Json<ApiResponse<String>> {
    Json(ApiResponse::new(String::new()))
}

async fn area_tree() -> Json<ApiResponse<Vec<Value>>> {
    #[derive(Clone)]
    struct AreaRow {
        id: i64,
        name: String,
    }

    fn children_of(parent_id: i64, rows: &HashMap<i64, Vec<AreaRow>>) -> Vec<Value> {
        rows.get(&parent_id)
            .into_iter()
            .flatten()
            .map(|area| {
                let children = children_of(area.id, rows);
                let mut value = json!({ "id": area.id, "name": area.name });
                if !children.is_empty() {
                    value["children"] = Value::Array(children);
                }
                value
            })
            .collect()
    }

    let mut rows: HashMap<i64, Vec<AreaRow>> = HashMap::new();
    for line in include_str!("../area.csv").lines().skip(1) {
        let mut fields = line.splitn(4, ',');
        let (Some(id), Some(name), Some(_area_type), Some(parent_id)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let (Ok(id), Ok(parent_id)) = (id.parse(), parent_id.parse()) else {
            continue;
        };
        rows.entry(parent_id).or_default().push(AreaRow {
            id,
            name: name.to_owned(),
        });
    }
    // Match Yudao's contract: return China's direct children, not the China root.
    Json(ApiResponse::new(children_of(1, &rows)))
}

async fn area_by_ip() -> Json<ApiResponse<String>> {
    Json(ApiResponse::new("未知".into()))
}

async fn oauth2_token_page(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    require(&user, "system:oauth2-token:page")?;
    yudao_page(
        &state.pool,
        YudaoSpec {
            table: "system_oauth2_access_token",
            seq: "system_oauth2_access_token_seq",
        },
        params,
    )
    .await
}

async fn oauth2_token_delete(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:oauth2-token:delete")?;
    yudao_delete(
        &state.pool,
        YudaoSpec {
            table: "system_oauth2_access_token",
            seq: "system_oauth2_access_token_seq",
        },
        params,
    )
    .await
}

async fn social_user_page(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    require(&user, "system:social-user:query")?;
    yudao_page(
        &state.pool,
        YudaoSpec {
            table: "system_social_user",
            seq: "system_social_user_seq",
        },
        params,
    )
    .await
}

async fn social_user_get(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "system:social-user:query")?;
    yudao_get(
        &state.pool,
        YudaoSpec {
            table: "system_social_user",
            seq: "system_social_user_seq",
        },
        params,
    )
    .await
}

async fn social_user_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "system:social-user:query")?;
    yudao_list(
        &state.pool,
        YudaoSpec {
            table: "system_social_user",
            seq: "system_social_user_seq",
        },
    )
    .await
}

async fn social_user_bind_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "system:social-user:query")?;
    yudao_list(
        &state.pool,
        YudaoSpec {
            table: "system_social_user_bind",
            seq: "system_social_user_bind_seq",
        },
    )
    .await
}

async fn user_page(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    require(&user, "system:user:query")?;
    let users =
        super::data_scope::visible_user_values(&state.pool, &user, current_tenant_id(&user)?)
            .await?;
    Ok(Json(ApiResponse::new(paginate(users, params))))
}

async fn user_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "system:user:list")?;
    Ok(Json(ApiResponse::new(
        super::data_scope::visible_user_values(&state.pool, &user, current_tenant_id(&user)?)
            .await?,
    )))
}

async fn user_simple_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "system:user:list")?;
    Ok(Json(ApiResponse::new(
        super::data_scope::visible_user_values(&state.pool, &user, current_tenant_id(&user)?)
            .await?,
    )))
}

async fn user_get(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "system:user:query")?;
    let id = params
        .get("id")
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("id is required"))?;
    let value =
        super::data_scope::visible_user_values(&state.pool, &user, current_tenant_id(&user)?)
            .await?
            .into_iter()
            .find(|value| value["id"] == id)
            .ok_or_else(|| AppError::not_found("user not found"))?;
    Ok(Json(ApiResponse::new(value)))
}

async fn user_create(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "system:user:create")?;
    let username = payload["username"].as_str().unwrap_or("").trim();
    let nickname = payload["nickname"]
        .as_str()
        .or_else(|| payload["displayName"].as_str())
        .unwrap_or(username);
    let password = payload["password"].as_str().unwrap_or("Admin#123456");
    let password_hash = state
        .passwords
        .hash_yudao(password)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let post_ids = i64_vec_field(&payload, "postIds");
    let role_ids = i64_vec_field(&payload, "roleIds");
    let tenant_id = user
        .tenant_id
        .as_deref()
        .and_then(|id| id.parse::<i64>().ok())
        .unwrap_or(0);
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin user creation"))?;
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO system_users
         (id, username, password, nickname, remark, dept_id, email, mobile, sex, avatar,
          status, creator, create_time, updater, update_time, deleted, tenant_id)
         VALUES (nextval('system_users_seq'), $1, $2, $3, $4, $5, $6, $7, $8, $9,
                 $10, $11, now(), $11, now(), 0, $12)
         RETURNING id",
    )
    .bind(username)
    .bind(password_hash)
    .bind(nickname)
    .bind(str_field(&payload, "remark"))
    .bind(opt_i64_field(&payload, "deptId"))
    .bind(str_field(&payload, "email"))
    .bind(str_field(&payload, "mobile"))
    .bind(i16_field(&payload, "sex", 1))
    .bind(str_field(&payload, "avatar"))
    .bind(i16_field(&payload, "status", 0))
    .bind(&user.username)
    .bind(tenant_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to create user"))?;
    super::user_relations::replace_posts(
        &mut transaction,
        id,
        &post_ids,
        &user.username,
        tenant_id,
    )
    .await
    .map_err(|_| AppError::bad_request("post does not exist"))?;
    super::user_relations::replace_roles(
        &mut transaction,
        id,
        &role_ids,
        &user.username,
        tenant_id,
    )
    .await
    .map_err(|_| AppError::bad_request("role does not exist"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit user creation"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn user_update(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:user:update")?;
    let id = parse_i64_value(&payload["id"])?;
    let nickname = payload["nickname"]
        .as_str()
        .or_else(|| payload["displayName"].as_str())
        .unwrap_or("");
    let tenant_id = current_tenant_id(&user)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin user update"))?;
    sqlx::query(
        "UPDATE system_users SET nickname=$2, remark=$3, dept_id=$4, email=$5, mobile=$6,
         sex=$7, avatar=$8, status=$9, updater=$10, update_time=now()
         WHERE id=$1 AND deleted=0 AND tenant_id=$11",
    )
    .bind(id)
    .bind(nickname)
    .bind(str_field(&payload, "remark"))
    .bind(opt_i64_field(&payload, "deptId"))
    .bind(str_field(&payload, "email"))
    .bind(str_field(&payload, "mobile"))
    .bind(i16_field(&payload, "sex", 1))
    .bind(str_field(&payload, "avatar"))
    .bind(i16_field(&payload, "status", 0))
    .bind(&user.username)
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to update user"))?;
    let tenant_id = sqlx::query_scalar::<_, i64>(
        "SELECT tenant_id FROM system_users WHERE id=$1 AND tenant_id=$2",
    )
    .bind(id)
    .bind(tenant_id)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| AppError::not_found("user not found"))?;
    if payload.get("postIds").is_some() {
        super::user_relations::replace_posts(
            &mut transaction,
            id,
            &i64_vec_field(&payload, "postIds"),
            &user.username,
            tenant_id,
        )
        .await
        .map_err(|_| AppError::bad_request("post does not exist"))?;
    }
    if payload.get("roleIds").is_some() {
        super::user_relations::replace_roles(
            &mut transaction,
            id,
            &i64_vec_field(&payload, "roleIds"),
            &user.username,
            tenant_id,
        )
        .await
        .map_err(|_| AppError::bad_request("role does not exist"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit user update"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn user_update_status(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:user:update")?;
    let id = parse_i64_value(&payload["id"])?;
    let tenant_id = current_tenant_id(&user)?;
    let status = i16_field(&payload, "status", 0);
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin user status update"))?;
    let updated = sqlx::query(
        "UPDATE system_users SET status = $2, update_time = now()
         WHERE id = $1 AND tenant_id = $3 AND deleted = 0",
    )
    .bind(id)
    .bind(status)
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to update user status"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("user not found"));
    }
    if status != 0 {
        crate::oauth2_token::revoke_user_tokens(&mut transaction, id)
            .await
            .map_err(|_| AppError::internal("failed to revoke disabled user tokens"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit user status update"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn user_update_password(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:user:update-password")?;
    let id = parse_i64_value(&payload["id"])?;
    let tenant_id = current_tenant_id(&user)?;
    let password = payload["password"]
        .as_str()
        .or_else(|| payload["newPassword"].as_str())
        .ok_or_else(|| AppError::bad_request("password is required"))?;
    let password_hash = state
        .passwords
        .hash_yudao(password)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin password update"))?;
    let updated = sqlx::query(
        "UPDATE system_users SET password = $2, update_time = now()
         WHERE id = $1 AND tenant_id = $3 AND deleted = 0",
    )
    .bind(id)
    .bind(password_hash)
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to update password"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("user not found"));
    }
    crate::oauth2_token::revoke_user_tokens(&mut transaction, id)
        .await
        .map_err(|_| AppError::internal("failed to revoke user tokens"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit password update"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn user_delete(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:user:delete")?;
    let id = parse_i64_param(&params, "id")?;
    let tenant_id = current_tenant_id(&user)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin user deletion"))?;
    let deleted = sqlx::query(
        "UPDATE system_users SET deleted=1, update_time=now()
         WHERE id = $1 AND tenant_id = $2 AND deleted = 0",
    )
    .bind(id)
    .bind(tenant_id)
    .execute(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to delete user"))?;
    if deleted.rows_affected() != 1 {
        return Err(AppError::not_found("user not found"));
    }
    crate::oauth2_token::revoke_user_tokens(&mut transaction, id)
        .await
        .map_err(|_| AppError::internal("failed to revoke deleted user tokens"))?;
    super::user_relations::delete_all(&mut transaction, id, &user.username)
        .await
        .map_err(|_| AppError::internal("failed to delete user relations"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit user deletion"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn user_delete_list(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:user:delete")?;
    let tenant_id = current_tenant_id(&user)?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin user deletion"))?;
    for id in split_i64_ids(&params) {
        let deleted = sqlx::query(
            "UPDATE system_users SET deleted=1, updater=$2, update_time=now()
             WHERE id = $1 AND tenant_id = $3 AND deleted = 0",
        )
        .bind(id)
        .bind(&user.username)
        .bind(tenant_id)
        .execute(&mut *transaction)
        .await
        .map_err(|_| AppError::internal("failed to delete user"))?;
        if deleted.rows_affected() != 1 {
            return Err(AppError::not_found("user not found"));
        }
        crate::oauth2_token::revoke_user_tokens(&mut transaction, id)
            .await
            .map_err(|_| AppError::internal("failed to revoke deleted user tokens"))?;
        super::user_relations::delete_all(&mut transaction, id, &user.username)
            .await
            .map_err(|_| AppError::internal("failed to delete user relations"))?;
    }
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit user deletion"))?;
    Ok(Json(ApiResponse::new(())))
}

async fn role_page(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    require(&user, "system:role:query")?;
    Ok(Json(ApiResponse::new(paginate(
        role_values(&state.pool, current_tenant_id(&user)?).await?,
        params,
    ))))
}

async fn role_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "system:role:query")?;
    Ok(Json(ApiResponse::new(
        role_values(&state.pool, current_tenant_id(&user)?).await?,
    )))
}

async fn role_get(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    require(&user, "system:role:query")?;
    let id = parse_i64_param(&params, "id")?;
    let value = role_values(&state.pool, current_tenant_id(&user)?)
        .await?
        .into_iter()
        .find(|value| value["id"] == id)
        .ok_or_else(|| AppError::not_found("role not found"))?;
    Ok(Json(ApiResponse::new(value)))
}

async fn role_create(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<String>>, AppError> {
    require(&user, "system:role:create")?;
    let tenant_id = current_tenant_id(&user)?;
    let data_scope_dept_ids = string_list_field(&payload, "dataScopeDeptIds");
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO system_role
         (id, code, name, sort, data_scope, data_scope_dept_ids, status, type, remark, creator, updater, tenant_id)
         VALUES (nextval('system_role_seq'), $1, $2, $3, $4, $5, $6, 2, $7, $8, $8, $9)
         RETURNING id",
    )
    .bind(payload["code"].as_str().unwrap_or(""))
    .bind(payload["name"].as_str().unwrap_or(""))
    .bind(i32_field(&payload, "sort", 0))
    .bind(i16_field(&payload, "dataScope", 1))
    .bind(data_scope_dept_ids)
    .bind(i16_field(&payload, "status", 0))
    .bind(opt_str_field(&payload, "remark"))
    .bind(&user.username)
    .bind(tenant_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| AppError::bad_request("role code already exists"))?;
    Ok(Json(ApiResponse::new(id.to_string())))
}

async fn role_update(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:role:update")?;
    role_update_unchecked(&state, payload, current_tenant_id(&user)?).await
}

async fn role_update_unchecked(
    state: &SystemState,
    payload: Value,
    tenant_id: i64,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let id = parse_i64_value(&payload["id"])?;
    let data_scope_dept_ids = string_list_field(&payload, "dataScopeDeptIds");
    let updated = sqlx::query("UPDATE system_role SET name=$2, code=$3, sort=$4, data_scope=$5, status=$6, remark=$7, data_scope_dept_ids=$8, update_time=now() WHERE id=$1 AND deleted=0 AND tenant_id=$9")
        .bind(id)
        .bind(payload["name"].as_str().unwrap_or(""))
        .bind(payload["code"].as_str().unwrap_or(""))
        .bind(i32_field(&payload, "sort", 0))
        .bind(i16_field(&payload, "dataScope", 1))
        .bind(i16_field(&payload, "status", 0))
        .bind(opt_str_field(&payload, "remark"))
        .bind(data_scope_dept_ids)
        .bind(tenant_id)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to update role"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("role not found"));
    }
    crate::cache::invalidate_all_current_users(state).await;
    Ok(Json(ApiResponse::new(())))
}

async fn role_delete(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:role:delete")?;
    let id = parse_i64_param(&params, "id")?;
    sqlx::query("UPDATE system_role SET deleted=1, update_time=now() WHERE id = $1 AND type <> 1 AND tenant_id=$2")
        .bind(id)
        .bind(current_tenant_id(&user)?)
        .execute(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to delete role"))?;
    crate::cache::invalidate_all_current_users(&state).await;
    Ok(Json(ApiResponse::new(())))
}

async fn role_delete_list(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:role:delete")?;
    let tenant_id = current_tenant_id(&user)?;
    for id in split_i64_ids(&params) {
        let _ = sqlx::query(
            "UPDATE system_role SET deleted=1, update_time=now() WHERE id = $1 AND type <> 1 AND tenant_id=$2",
        )
        .bind(id)
        .bind(tenant_id)
        .execute(&state.pool)
        .await;
    }
    crate::cache::invalidate_all_current_users(&state).await;
    Ok(Json(ApiResponse::new(())))
}

async fn role_values(pool: &PgPool, tenant_id: i64) -> Result<Vec<Value>, AppError> {
    sqlx::query_scalar::<_, Value>(
        "SELECT jsonb_build_object(
            'id', id,
            'name', name,
            'code', code,
            'sort', sort,
            'status', status,
            'type', type,
            'remark', remark,
            'dataScope', data_scope,
            'dataScopeDeptIds', COALESCE(NULLIF(data_scope_dept_ids, ''), '[]')::jsonb,
            'createTime', create_time
         )
         FROM system_role
         WHERE deleted = 0 AND tenant_id = $1
         ORDER BY type, sort, id",
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to list roles"))
}

async fn permission_user_roles(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<String>>>, AppError> {
    require(&user, "system:permission:assign-user-role")?;
    let user_id = params
        .get("userId")
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("userId is required"))?;
    let ids = sqlx::query_scalar::<_, i64>(
        "SELECT role_id FROM system_user_role WHERE user_id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list user roles"))?;
    Ok(Json(ApiResponse::new(
        ids.into_iter().map(|id| id.to_string()).collect(),
    )))
}

async fn permission_assign_user_roles(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:permission:assign-user-role")?;
    let user_id = parse_i64_value(&payload["userId"])?;
    let role_ids = i64_vec_field(&payload, "roleIds");
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to assign roles"))?;
    let tenant_id = sqlx::query_scalar::<_, i64>(
        "SELECT tenant_id FROM system_users WHERE id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| AppError::not_found("user not found"))?;
    super::user_relations::replace_roles(&mut tx, user_id, &role_ids, &user.username, tenant_id)
        .await
        .map_err(|_| AppError::bad_request("role does not exist"))?;
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to assign roles"))?;
    if let Ok(cache_user_id) = uuid_for_yudao_user_id(&state.pool, user_id).await {
        crate::cache::invalidate_current_user(&state, cache_user_id).await;
    }
    Ok(Json(ApiResponse::new(())))
}

async fn permission_role_menus(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ApiResponse<Vec<i64>>>, AppError> {
    require(&user, "system:permission:assign-role-menu")?;
    let role_id = params
        .get("roleId")
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("roleId is required"))?;
    let ids = sqlx::query_scalar::<_, i64>(
        "SELECT menu_id FROM system_role_menu WHERE role_id = $1 AND deleted=0 ORDER BY menu_id",
    )
    .bind(role_id)
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list role menus"))?;
    Ok(Json(ApiResponse::new(ids)))
}

async fn permission_assign_role_menus(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:permission:assign-role-menu")?;
    let role_id = parse_i64_value(&payload["roleId"])?;
    let menu_ids = payload["menuIds"].as_array().cloned().unwrap_or_default();
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to assign role menus"))?;
    sqlx::query("UPDATE system_role_menu SET deleted=1, update_time=now() WHERE role_id = $1")
        .bind(role_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| AppError::internal("failed to assign role menus"))?;
    for menu_id in menu_ids {
        let Some(menu_id) = menu_id
            .as_i64()
            .or_else(|| menu_id.as_str().and_then(|id| id.parse::<i64>().ok()))
        else {
            continue;
        };
        sqlx::query("INSERT INTO system_role_menu (id, role_id, menu_id, deleted) VALUES (nextval('system_role_menu_seq'), $1, $2, 0)")
            .bind(role_id)
            .bind(menu_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| AppError::bad_request("menu does not exist"))?;
    }
    tx.commit()
        .await
        .map_err(|_| AppError::internal("failed to assign role menus"))?;
    crate::cache::invalidate_all_current_users(&state).await;
    Ok(Json(ApiResponse::new(())))
}

async fn role_update_data_scope(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    require(&user, "system:permission:assign-role-data-scope")?;
    let id = payload
        .get("roleId")
        .or_else(|| payload.get("id"))
        .ok_or_else(|| AppError::bad_request("roleId is required"))
        .and_then(parse_i64_value)?;
    let updated = sqlx::query(
        "UPDATE system_role
         SET data_scope=$2, data_scope_dept_ids=$3, update_time=now()
         WHERE id=$1 AND deleted=0 AND tenant_id=$4",
    )
    .bind(id)
    .bind(i16_field(&payload, "dataScope", 1))
    .bind(string_list_field(&payload, "dataScopeDeptIds"))
    .bind(current_tenant_id(&user)?)
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update role data scope"))?;
    if updated.rows_affected() != 1 {
        return Err(AppError::not_found("role not found"));
    }
    crate::cache::invalidate_all_current_users(&state).await;
    Ok(Json(ApiResponse::new(())))
}

async fn menu_list(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Vec<Value>>>, AppError> {
    require(&user, "system:menu:query")?;
    core_list(&state.pool, "menu").await
}

async fn operate_log_page(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    require(&user, "system:operate-log:query")?;
    let rows = sqlx::query_as::<_, AuditCompatRow>(
        "SELECT logs.id,
                NULLIF(logs.user_id, 0) AS actor_user_id,
                users.username AS actor_username,
                logs.sub_type AS action,
                logs.type AS target_type,
                NULLIF(logs.biz_id, 0)::text AS target_id,
                CASE WHEN logs.action ~ '^\\s*[\\[{]'
                     THEN logs.action::jsonb ELSE to_jsonb(logs.action) END AS detail,
                logs.create_time AT TIME ZONE 'UTC' AS created_at
         FROM system_operate_log logs
         LEFT JOIN system_users users ON users.id = logs.user_id AND users.deleted = 0
         WHERE logs.deleted = 0
         ORDER BY logs.create_time DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list operate logs"))?;
    let values = rows.into_iter().map(audit_value).collect();
    Ok(Json(ApiResponse::new(paginate(values, params))))
}

async fn login_log_page(
    user: CurrentUser,
    State(state): State<SystemState>,
    Query(params): Query<QueryParams>,
) -> Result<Json<ApiResponse<Page<Value>>>, AppError> {
    require(&user, "system:login-log:query")?;
    let rows = sqlx::query_as::<_, LoginLogCompatRow>(
        "SELECT id, log_type, trace_id, NULLIF(user_id, 0) AS user_id, user_type,
                username, result, user_ip, user_agent, tenant_id,
                create_time AT TIME ZONE 'UTC' AS create_time
         FROM system_login_log
         WHERE deleted = 0
         ORDER BY create_time DESC",
    )
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to list login logs"))?;
    let values = rows.into_iter().map(login_log_value).collect();
    Ok(Json(ApiResponse::new(paginate(values, params))))
}

#[derive(FromRow)]
struct LoginLogCompatRow {
    id: i64,
    log_type: i64,
    trace_id: String,
    user_id: Option<i64>,
    user_type: i16,
    username: String,
    result: i16,
    user_ip: String,
    user_agent: String,
    tenant_id: i64,
    create_time: DateTime<Utc>,
}

fn login_log_value(row: LoginLogCompatRow) -> Value {
    json!({
        "id": row.id,
        "logType": row.log_type,
        "traceId": row.trace_id,
        "userId": row.user_id,
        "userType": row.user_type,
        "username": row.username,
        "result": row.result,
        "userIp": row.user_ip,
        "userAgent": row.user_agent,
        "tenantId": row.tenant_id,
        "createTime": row.create_time.to_rfc3339(),
    })
}

#[derive(FromRow)]
struct AuditCompatRow {
    id: i64,
    actor_user_id: Option<i64>,
    actor_username: Option<String>,
    action: String,
    target_type: String,
    target_id: Option<String>,
    detail: Value,
    created_at: DateTime<Utc>,
}

fn audit_value(row: AuditCompatRow) -> Value {
    json!({
        "id": row.id.to_string(),
        "traceId": row.id.to_string(),
        "userType": 2,
        "userId": row.actor_user_id.map(|id| id.to_string()),
        "userName": row.actor_username.clone().unwrap_or_else(|| "system".into()),
        "username": row.actor_username.unwrap_or_else(|| "system".into()),
        "type": row.target_type,
        "subType": row.action,
        "bizId": row.target_id,
        "action": row.detail,
        "extra": "",
        "requestMethod": "",
        "requestUrl": "",
        "userIp": "",
        "userAgent": "",
        "creator": "system",
        "creatorName": "system",
        "result": 0,
        "status": 0,
        "createTime": row.created_at.to_rfc3339()
    })
}

async fn profile_get(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Json<ApiResponse<Value>>, AppError> {
    let mut value =
        super::data_scope::visible_user_values(&state.pool, &user, current_tenant_id(&user)?)
            .await?
            .into_iter()
            .find(|value| value["username"] == user.username)
            .ok_or_else(|| AppError::not_found("user not found"))?;
    let dept = value["deptId"].as_i64().map(|id| {
        json!({
            "id": id,
            "name": value["deptName"].as_str().unwrap_or_default(),
        })
    });
    let posts = if let Some(user_id) = value["id"].as_i64() {
        sqlx::query_scalar::<_, Value>(
            "SELECT jsonb_build_object('id', post.id, 'name', post.name, 'code', post.code)
             FROM system_user_post relation
             JOIN system_post post ON post.id = relation.post_id AND post.deleted = 0
             WHERE relation.user_id = $1 AND relation.deleted = 0
             ORDER BY post.sort, post.id",
        )
        .bind(user_id)
        .fetch_all(&state.pool)
        .await
        .map_err(|_| AppError::internal("failed to list user posts"))?
    } else {
        Vec::new()
    };
    value["dept"] = dept.map_or(Value::Null, Value::from);
    value["posts"] = json!(posts);
    value["roles"] = json!(user.role_codes);
    value["socialUsers"] = json!([]);
    Ok(Json(ApiResponse::new(value)))
}

async fn profile_update(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    sqlx::query(
        "UPDATE system_users
         SET nickname = coalesce($2, nickname),
             email = coalesce($3, email),
             mobile = coalesce($4, mobile),
             sex = coalesce($5, sex),
             avatar = coalesce($6, avatar),
             update_time = now()
         WHERE username = $1 AND deleted = 0",
    )
    .bind(&user.username)
    .bind(opt_str_field(&payload, "nickname"))
    .bind(opt_str_field(&payload, "email"))
    .bind(opt_str_field(&payload, "mobile"))
    .bind(opt_i16_field(&payload, "sex"))
    .bind(opt_str_field(&payload, "avatar"))
    .execute(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to update profile"))?;

    if let Ok(id) = parse_id(&user.user_id) {
        crate::cache::invalidate_current_user(&state, id).await;
    }
    Ok(Json(ApiResponse::new(())))
}

async fn profile_password(
    user: CurrentUser,
    State(state): State<SystemState>,
    Json(payload): Json<Value>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    let password = payload["newPassword"]
        .as_str()
        .or_else(|| payload["password"].as_str())
        .ok_or_else(|| AppError::bad_request("password is required"))?;
    let password_hash = state
        .passwords
        .hash_yudao(password)
        .map_err(|error| AppError::bad_request(error.to_string()))?;
    let mut transaction = state
        .pool
        .begin()
        .await
        .map_err(|_| AppError::internal("failed to begin password update"))?;
    let user_id = sqlx::query_scalar::<_, i64>(
        "UPDATE system_users SET password = $2, update_time = now()
         WHERE username = $1 AND deleted = 0
         RETURNING id",
    )
    .bind(&user.username)
    .bind(password_hash)
    .fetch_one(&mut *transaction)
    .await
    .map_err(|_| AppError::internal("failed to update password"))?;
    crate::oauth2_token::revoke_user_tokens(&mut transaction, user_id)
        .await
        .map_err(|_| AppError::internal("failed to revoke user tokens"))?;
    transaction
        .commit()
        .await
        .map_err(|_| AppError::internal("failed to commit password update"))?;
    if let Ok(id) = parse_id(&user.user_id) {
        crate::cache::invalidate_current_user(&state, id).await;
    }
    Ok(Json(ApiResponse::new(())))
}

fn paginate(values: Vec<Value>, params: QueryParams) -> Page<Value> {
    let total = values.len() as i64;
    let page_no = params.page_no.unwrap_or(1).max(1) as usize;
    let page_size = params.page_size.unwrap_or(10).clamp(1, 200) as usize;
    let start = (page_no - 1) * page_size;
    Page {
        list: values.into_iter().skip(start).take(page_size).collect(),
        total,
    }
}
