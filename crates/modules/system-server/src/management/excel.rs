use std::io::Cursor;

use axum::{
    Json,
    body::Body,
    extract::{Multipart, State},
    response::Response,
};
use calamine::{Data, Reader, open_workbook_auto_from_rs};
use rust_toon_framework_common::ApiResponse;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use rust_xlsxwriter::Workbook;
use serde_json::{Value, json};

use crate::{SystemState, management::shared::require};

type ImportResult = Json<ApiResponse<Value>>;

const USER_IMPORT_COLUMNS: &[(&str, &str)] = &[
    ("username", "用户名"),
    ("nickname", "用户昵称"),
    ("password", "密码"),
    ("deptId", "部门编号"),
    ("email", "邮箱"),
    ("mobile", "手机号"),
    ("sex", "性别"),
    ("postIds", "岗位编号，逗号分隔"),
    ("roleIds", "角色编号，逗号分隔"),
    ("status", "状态"),
    ("remark", "备注"),
];

pub async fn user_export(
    user: CurrentUser,
    State(state): State<SystemState>,
) -> Result<Response, AppError> {
    require(&user, "system:user:export")?;
    let tenant_id = current_tenant_id(&user)?;
    let rows = super::data_scope::visible_user_values(&state.pool, &user, tenant_id).await?;
    workbook_response(
        "system-user.xlsx",
        "用户",
        &[
            ("id", "编号"),
            ("username", "用户名"),
            ("nickname", "昵称"),
            ("deptId", "部门编号"),
            ("deptName", "部门"),
            ("email", "邮箱"),
            ("mobile", "手机号"),
            ("sex", "性别"),
            ("status", "状态"),
            ("remark", "备注"),
            ("createTime", "创建时间"),
        ],
        &rows,
    )
}

pub async fn user_import_template() -> Result<Response, AppError> {
    workbook_response(
        "system-user-import-template.xlsx",
        "用户导入",
        USER_IMPORT_COLUMNS,
        &[],
    )
}

pub async fn user_import(
    user: CurrentUser,
    State(state): State<SystemState>,
    mut multipart: Multipart,
) -> Result<ImportResult, AppError> {
    require(&user, "system:user:import")?;
    let tenant_id = current_tenant_id(&user)?;
    let mut file = None;
    let mut update_support = false;
    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|_| AppError::bad_request("invalid multipart form"))?
    {
        match field.name().unwrap_or_default() {
            "file" => {
                file = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|_| AppError::bad_request("failed to read upload file"))?
                        .to_vec(),
                );
            }
            "updateSupport" => {
                let value = field.text().await.unwrap_or_default();
                update_support = value == "true" || value == "1";
            }
            _ => {}
        }
    }
    let file = file.ok_or_else(|| AppError::bad_request("file is required"))?;
    let records = read_user_import_rows(&file)?;
    let mut create_usernames = Vec::new();
    let mut update_usernames = Vec::new();
    let mut failure_usernames = serde_json::Map::new();

    for record in records {
        let username = record.username.trim();
        if username.is_empty() {
            continue;
        }
        match import_one_user(&state, &user, tenant_id, &record, update_support).await {
            Ok(ImportAction::Created) => create_usernames.push(username.to_owned()),
            Ok(ImportAction::Updated) => update_usernames.push(username.to_owned()),
            Err(error) => {
                failure_usernames.insert(username.to_owned(), json!(error));
            }
        }
    }

    Ok(Json(ApiResponse::new(json!({
        "createUsernames": create_usernames,
        "updateUsernames": update_usernames,
        "failureUsernames": failure_usernames
    }))))
}

macro_rules! export_handler {
    ($name:ident, $kind:literal) => {
        pub async fn $name(
            user: CurrentUser,
            State(state): State<SystemState>,
        ) -> Result<Response, AppError> {
            export_kind(user, state, $kind).await
        }
    };
}

export_handler!(role_export, "role");
export_handler!(post_export, "post");
export_handler!(dict_type_export, "dict_type");
export_handler!(dict_data_export, "dict_data");
export_handler!(tenant_export, "tenant");
export_handler!(operate_log_export, "operate_log");
export_handler!(login_log_export, "login_log");
export_handler!(notify_template_export, "notify_template");
export_handler!(mail_log_export, "mail_log");
export_handler!(sms_channel_export, "sms_channel");
export_handler!(sms_template_export, "sms_template");
export_handler!(sms_log_export, "sms_log");

async fn export_kind(
    user: CurrentUser,
    state: SystemState,
    kind: &str,
) -> Result<Response, AppError> {
    /// sqlx 0.9 要求动态 SQL 显式声明已审计：插值部分是 `export_spec` 静态
    /// 匹配表里的表名（编译期字面量），非用户输入。
    fn audited_sql(sql: impl Into<String>) -> sqlx::AssertSqlSafe<String> {
        sqlx::AssertSqlSafe(sql.into())
    }

    let spec = export_spec(kind).ok_or_else(|| AppError::bad_request("unsupported export kind"))?;
    require(&user, spec.permission)?;
    let rows = if spec.tenant_scoped {
        sqlx::query_scalar::<_, Value>(audited_sql(format!(
            "SELECT to_jsonb(t) FROM {} t WHERE deleted=0 AND tenant_id=$1 ORDER BY id DESC LIMIT 10000",
            spec.table
        )))
        .bind(current_tenant_id(&user)?)
        .fetch_all(&state.pool)
        .await
    } else {
        sqlx::query_scalar::<_, Value>(audited_sql(format!(
            "SELECT to_jsonb(t) FROM {} t WHERE deleted=0 ORDER BY id DESC LIMIT 10000",
            spec.table
        )))
        .fetch_all(&state.pool)
        .await
    }
    .map_err(|_| AppError::internal("failed to export records"))?;
    let rows: Vec<Value> = rows.into_iter().map(snake_object_to_camel).collect();
    workbook_response(spec.filename, spec.sheet_name, spec.columns, &rows)
}

struct ExportSpec {
    table: &'static str,
    filename: &'static str,
    sheet_name: &'static str,
    permission: &'static str,
    columns: &'static [(&'static str, &'static str)],
    tenant_scoped: bool,
}

fn export_spec(kind: &str) -> Option<ExportSpec> {
    let spec = match kind {
        "role" => ExportSpec {
            table: "system_role",
            filename: "system-role.xlsx",
            sheet_name: "角色",
            permission: "system:role:export",
            tenant_scoped: true,
            columns: &[
                ("id", "编号"),
                ("name", "角色名称"),
                ("code", "角色标识"),
                ("sort", "排序"),
                ("dataScope", "数据范围"),
                ("status", "状态"),
                ("type", "类型"),
                ("remark", "备注"),
                ("createTime", "创建时间"),
            ],
        },
        "post" => ExportSpec {
            table: "system_post",
            filename: "system-post.xlsx",
            sheet_name: "岗位",
            permission: "system:post:export",
            tenant_scoped: true,
            columns: &[
                ("id", "编号"),
                ("name", "岗位名称"),
                ("code", "岗位编码"),
                ("sort", "排序"),
                ("status", "状态"),
                ("remark", "备注"),
                ("createTime", "创建时间"),
            ],
        },
        "dict_type" => ExportSpec {
            table: "system_dict_type",
            filename: "system-dict-type.xlsx",
            sheet_name: "字典类型",
            permission: "system:dict:export",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("name", "字典名称"),
                ("type", "字典类型"),
                ("status", "状态"),
                ("remark", "备注"),
                ("createTime", "创建时间"),
            ],
        },
        "dict_data" => ExportSpec {
            table: "system_dict_data",
            filename: "system-dict-data.xlsx",
            sheet_name: "字典数据",
            permission: "system:dict:export",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("sort", "排序"),
                ("label", "标签"),
                ("value", "键值"),
                ("dictType", "字典类型"),
                ("status", "状态"),
                ("createTime", "创建时间"),
            ],
        },
        "tenant" => ExportSpec {
            table: "system_tenant",
            filename: "system-tenant.xlsx",
            sheet_name: "租户",
            permission: "system:tenant:export",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("name", "租户名"),
                ("contactName", "联系人"),
                ("contactMobile", "联系电话"),
                ("status", "状态"),
                ("packageId", "套餐"),
                ("expireTime", "过期时间"),
                ("accountCount", "账号额度"),
                ("createTime", "创建时间"),
            ],
        },
        "operate_log" => ExportSpec {
            table: "system_operate_log",
            filename: "system-operate-log.xlsx",
            sheet_name: "操作日志",
            permission: "system:operate-log:export",
            tenant_scoped: true,
            columns: &[
                ("id", "编号"),
                ("traceId", "链路编号"),
                ("userId", "用户编号"),
                ("type", "类型"),
                ("subType", "子类型"),
                ("action", "操作"),
                ("success", "是否成功"),
                ("createTime", "创建时间"),
            ],
        },
        "login_log" => ExportSpec {
            table: "system_login_log",
            filename: "system-login-log.xlsx",
            sheet_name: "登录日志",
            permission: "system:login-log:export",
            tenant_scoped: true,
            columns: &[
                ("id", "编号"),
                ("username", "用户名"),
                ("logType", "日志类型"),
                ("result", "结果"),
                ("userIp", "用户 IP"),
                ("userAgent", "User-Agent"),
                ("createTime", "创建时间"),
            ],
        },
        "notify_template" => ExportSpec {
            table: "system_notify_template",
            filename: "system-notify-template.xlsx",
            sheet_name: "站内信模板",
            permission: "system:notify-template:export",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("name", "模板名称"),
                ("code", "模板编码"),
                ("nickname", "发送人"),
                ("content", "内容"),
                ("type", "类型"),
                ("status", "状态"),
                ("createTime", "创建时间"),
            ],
        },
        "mail_log" => ExportSpec {
            table: "system_mail_log",
            filename: "system-mail-log.xlsx",
            sheet_name: "邮件日志",
            permission: "system:mail-log:query",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("toMails", "收件人"),
                ("templateTitle", "主题"),
                ("sendStatus", "发送状态"),
                ("createTime", "创建时间"),
            ],
        },
        "sms_channel" => ExportSpec {
            table: "system_sms_channel",
            filename: "system-sms-channel.xlsx",
            sheet_name: "短信渠道",
            permission: "system:sms-channel:query",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("signature", "签名"),
                ("code", "编码"),
                ("status", "状态"),
                ("createTime", "创建时间"),
            ],
        },
        "sms_template" => ExportSpec {
            table: "system_sms_template",
            filename: "system-sms-template.xlsx",
            sheet_name: "短信模板",
            permission: "system:sms-template:export",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("name", "模板名称"),
                ("code", "编码"),
                ("content", "内容"),
                ("status", "状态"),
                ("createTime", "创建时间"),
            ],
        },
        "sms_log" => ExportSpec {
            table: "system_sms_log",
            filename: "system-sms-log.xlsx",
            sheet_name: "短信日志",
            permission: "system:sms-log:export",
            tenant_scoped: false,
            columns: &[
                ("id", "编号"),
                ("mobile", "手机号"),
                ("templateCode", "模板编码"),
                ("sendStatus", "发送状态"),
                ("createTime", "创建时间"),
            ],
        },
        _ => return None,
    };
    Some(spec)
}

#[derive(Debug)]
struct UserImportRecord {
    username: String,
    nickname: String,
    password: String,
    dept_id: Option<i64>,
    email: String,
    mobile: String,
    sex: i16,
    post_ids: Vec<i64>,
    role_ids: Vec<i64>,
    status: i16,
    remark: String,
}

enum ImportAction {
    Created,
    Updated,
}

async fn import_one_user(
    state: &SystemState,
    user: &CurrentUser,
    tenant_id: i64,
    record: &UserImportRecord,
    update_support: bool,
) -> Result<ImportAction, String> {
    let existing_id = sqlx::query_scalar::<_, i64>(
        "SELECT id FROM system_users WHERE username=$1 AND tenant_id=$2 AND deleted=0",
    )
    .bind(&record.username)
    .bind(tenant_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(|_| "查询用户失败".to_owned())?;

    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| "开启事务失败".to_owned())?;
    if let Some(id) = existing_id {
        if !update_support {
            return Err("用户已存在".to_owned());
        }
        sqlx::query(
            "UPDATE system_users
             SET nickname=$2, remark=$3, dept_id=$4, email=$5, mobile=$6,
                 sex=$7, status=$8, updater=$9, update_time=now()
             WHERE id=$1 AND tenant_id=$10 AND deleted=0",
        )
        .bind(id)
        .bind(&record.nickname)
        .bind(&record.remark)
        .bind(record.dept_id)
        .bind(&record.email)
        .bind(&record.mobile)
        .bind(record.sex)
        .bind(record.status)
        .bind(&user.username)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| "更新用户失败".to_owned())?;
        super::user_relations::replace_posts(
            &mut tx,
            id,
            &record.post_ids,
            &user.username,
            tenant_id,
        )
        .await
        .map_err(|_| "岗位不存在".to_owned())?;
        super::user_relations::replace_roles(&mut tx, Some(user), id, &record.role_ids, tenant_id)
            .await
            .map_err(|_| "角色不存在".to_owned())?;
        tx.commit().await.map_err(|_| "提交事务失败".to_owned())?;
        return Ok(ImportAction::Updated);
    }

    let password = if record.password.is_empty() {
        "Admin#123456"
    } else {
        &record.password
    };
    let password_hash = state
        .passwords
        .hash_yudao(password)
        .map_err(|_| "密码格式不合法".to_owned())?;
    let id = sqlx::query_scalar::<_, i64>(
        "INSERT INTO system_users
         (id, username, password, nickname, remark, dept_id, email, mobile, sex, avatar,
          status, creator, create_time, updater, update_time, deleted, tenant_id)
         VALUES (nextval('system_users_seq'), $1, $2, $3, $4, $5, $6, $7, $8, '',
                 $9, $10, now(), $10, now(), 0, $11)
         RETURNING id",
    )
    .bind(&record.username)
    .bind(password_hash)
    .bind(&record.nickname)
    .bind(&record.remark)
    .bind(record.dept_id)
    .bind(&record.email)
    .bind(&record.mobile)
    .bind(record.sex)
    .bind(record.status)
    .bind(&user.username)
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| "创建用户失败".to_owned())?;
    super::user_relations::replace_posts(&mut tx, id, &record.post_ids, &user.username, tenant_id)
        .await
        .map_err(|_| "岗位不存在".to_owned())?;
    super::user_relations::replace_roles(&mut tx, Some(user), id, &record.role_ids, tenant_id)
        .await
        .map_err(|_| "角色不存在".to_owned())?;
    tx.commit().await.map_err(|_| "提交事务失败".to_owned())?;
    Ok(ImportAction::Created)
}

fn read_user_import_rows(bytes: &[u8]) -> Result<Vec<UserImportRecord>, AppError> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut workbook = open_workbook_auto_from_rs(cursor)
        .map_err(|_| AppError::bad_request("invalid excel file"))?;
    let sheet = workbook
        .sheet_names()
        .first()
        .cloned()
        .ok_or_else(|| AppError::bad_request("excel has no sheets"))?;
    let range = workbook
        .worksheet_range(&sheet)
        .map_err(|_| AppError::bad_request("failed to read excel sheet"))?;
    Ok(range
        .rows()
        .skip(1)
        .map(|row| UserImportRecord {
            username: cell_string(row.first()),
            nickname: cell_string(row.get(1)),
            password: cell_string(row.get(2)),
            dept_id: cell_i64(row.get(3)),
            email: cell_string(row.get(4)),
            mobile: cell_string(row.get(5)),
            sex: cell_i64(row.get(6)).unwrap_or(1) as i16,
            post_ids: split_ids(&cell_string(row.get(7))),
            role_ids: split_ids(&cell_string(row.get(8))),
            status: cell_i64(row.get(9)).unwrap_or(0) as i16,
            remark: cell_string(row.get(10)),
        })
        .collect())
}

fn workbook_response(
    filename: &str,
    sheet_name: &str,
    columns: &[(&str, &str)],
    rows: &[Value],
) -> Result<Response, AppError> {
    let mut workbook = Workbook::new();
    let worksheet = workbook.add_worksheet();
    worksheet.set_name(sheet_name).ok();
    for (col, (_, title)) in columns.iter().enumerate() {
        worksheet
            .write_string(0, col as u16, *title)
            .map_err(|_| AppError::internal("failed to write excel header"))?;
    }
    for (row_index, row) in rows.iter().enumerate() {
        for (col_index, (key, _)) in columns.iter().enumerate() {
            worksheet
                .write_string(
                    (row_index + 1) as u32,
                    col_index as u16,
                    value_string(row.get(*key)),
                )
                .map_err(|_| AppError::internal("failed to write excel row"))?;
        }
    }
    let bytes = workbook
        .save_to_buffer()
        .map_err(|_| AppError::internal("failed to build excel"))?;
    Response::builder()
        .header(
            "content-type",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        )
        .header(
            "content-disposition",
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from(bytes))
        .map_err(|_| AppError::internal("failed to build download"))
}

fn snake_object_to_camel(value: Value) -> Value {
    let Value::Object(object) = value else {
        return value;
    };
    Value::Object(
        object
            .into_iter()
            .filter(|(key, _)| key != "deleted")
            .map(|(key, value)| (snake_to_camel(&key), parse_jsonish_value(value)))
            .collect(),
    )
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

fn parse_jsonish_value(value: Value) -> Value {
    let Value::String(text) = &value else {
        return value;
    };
    if !(text.starts_with('{') || text.starts_with('[')) {
        return value;
    }
    serde_json::from_str(text).unwrap_or(value)
}

fn value_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::Null) | None => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(Value::Number(value)) => value.to_string(),
        Some(Value::Bool(value)) => value.to_string(),
        Some(Value::Array(value)) => value
            .iter()
            .map(|v| value_string(Some(v)))
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => value.unwrap().to_string(),
    }
}

fn cell_string(cell: Option<&Data>) -> String {
    match cell {
        Some(Data::String(value)) => value.trim().to_owned(),
        Some(Data::Int(value)) => value.to_string(),
        Some(Data::Float(value)) if value.fract() == 0.0 => (*value as i64).to_string(),
        Some(Data::Float(value)) => value.to_string(),
        Some(Data::Bool(value)) => value.to_string(),
        Some(Data::DateTimeIso(value)) | Some(Data::DurationIso(value)) => value.clone(),
        Some(Data::DateTime(value)) => value.to_string(),
        Some(Data::Error(value)) => value.to_string(),
        Some(Data::Empty) | None => String::new(),
    }
}

fn cell_i64(cell: Option<&Data>) -> Option<i64> {
    match cell {
        Some(Data::Int(value)) => Some(*value),
        Some(Data::Float(value)) => Some(*value as i64),
        _ => cell_string(cell).parse::<i64>().ok(),
    }
}

fn split_ids(value: &str) -> Vec<i64> {
    value
        .split(',')
        .filter_map(|value| value.trim().parse::<i64>().ok())
        .collect()
}

fn current_tenant_id(user: &CurrentUser) -> Result<i64, AppError> {
    user.tenant_id
        .as_deref()
        .and_then(|id| id.parse::<i64>().ok())
        .ok_or_else(|| AppError::bad_request("tenant is required"))
}
