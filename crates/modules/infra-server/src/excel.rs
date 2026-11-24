use axum::{body::Body, extract::State, response::Response};
use rust_toon_framework_web::AppError;
use rust_xlsxwriter::Workbook;
use serde_json::Value;

use crate::InfraState;

macro_rules! export_handler {
    ($name:ident, $kind:literal) => {
        pub async fn $name(State(state): State<InfraState>) -> Result<Response, AppError> {
            export_kind(state, $kind).await
        }
    };
}

export_handler!(config_export, "config");
export_handler!(job_export, "job");
export_handler!(job_log_export, "job_log");
export_handler!(api_access_log_export, "api_access_log");
export_handler!(api_error_log_export, "api_error_log");
export_handler!(demo01_contact_export, "demo01_contact");
export_handler!(demo02_category_export, "demo02_category");
export_handler!(demo03_student_export, "demo03_student");

async fn export_kind(state: InfraState, kind: &str) -> Result<Response, AppError> {
    let spec = export_spec(kind).ok_or_else(|| AppError::bad_request("unsupported export kind"))?;
    // spec.table 来自静态规格表，非用户输入；sqlx 0.9 要求动态 SQL 显式声明已审计。
    let rows = sqlx::query_scalar::<_, Value>(sqlx::AssertSqlSafe(format!(
        "SELECT to_jsonb(t) FROM {} t WHERE deleted=0 ORDER BY id DESC LIMIT 10000",
        spec.table
    )))
    .fetch_all(&state.pool)
    .await
    .map_err(|_| AppError::internal("failed to export records"))?;
    let rows = rows
        .into_iter()
        .map(snake_object_to_camel)
        .collect::<Vec<_>>();
    workbook_response(spec.filename, spec.sheet_name, spec.columns, &rows)
}

struct ExportSpec {
    table: &'static str,
    filename: &'static str,
    sheet_name: &'static str,
    columns: &'static [(&'static str, &'static str)],
}

fn export_spec(kind: &str) -> Option<ExportSpec> {
    let spec = match kind {
        "config" => ExportSpec {
            table: "infra_config",
            filename: "infra-config.xlsx",
            sheet_name: "参数配置",
            columns: &[
                ("id", "编号"),
                ("category", "分类"),
                ("type", "类型"),
                ("name", "参数名称"),
                ("configKey", "参数键名"),
                ("value", "参数键值"),
                ("visible", "是否可见"),
                ("remark", "备注"),
                ("createTime", "创建时间"),
            ],
        },
        "job" => ExportSpec {
            table: "infra_job",
            filename: "infra-job.xlsx",
            sheet_name: "定时任务",
            columns: &[
                ("id", "编号"),
                ("name", "任务名称"),
                ("status", "状态"),
                ("handlerName", "处理器"),
                ("handlerParam", "处理参数"),
                ("cronExpression", "Cron 表达式"),
                ("retryCount", "重试次数"),
                ("retryInterval", "重试间隔"),
                ("monitorTimeout", "监控超时"),
                ("createTime", "创建时间"),
            ],
        },
        "job_log" => ExportSpec {
            table: "infra_job_log",
            filename: "infra-job-log.xlsx",
            sheet_name: "任务日志",
            columns: &[
                ("id", "编号"),
                ("jobId", "任务编号"),
                ("handlerName", "处理器"),
                ("handlerParam", "处理参数"),
                ("executeIndex", "执行次数"),
                ("beginTime", "开始时间"),
                ("endTime", "结束时间"),
                ("duration", "执行时长"),
                ("status", "状态"),
                ("result", "结果"),
                ("createTime", "创建时间"),
            ],
        },
        "api_access_log" => ExportSpec {
            table: "infra_api_access_log",
            filename: "infra-api-access-log.xlsx",
            sheet_name: "API 访问日志",
            columns: &[
                ("id", "编号"),
                ("traceId", "链路编号"),
                ("userId", "用户编号"),
                ("applicationName", "应用名"),
                ("requestMethod", "请求方法"),
                ("requestUrl", "请求地址"),
                ("userIp", "用户 IP"),
                ("operateModule", "操作模块"),
                ("operateName", "操作名"),
                ("duration", "耗时"),
                ("resultCode", "结果码"),
                ("resultMsg", "结果消息"),
                ("createTime", "创建时间"),
            ],
        },
        "api_error_log" => ExportSpec {
            table: "infra_api_error_log",
            filename: "infra-api-error-log.xlsx",
            sheet_name: "API 错误日志",
            columns: &[
                ("id", "编号"),
                ("traceId", "链路编号"),
                ("userId", "用户编号"),
                ("applicationName", "应用名"),
                ("requestMethod", "请求方法"),
                ("requestUrl", "请求地址"),
                ("userIp", "用户 IP"),
                ("exceptionTime", "异常时间"),
                ("exceptionName", "异常名"),
                ("exceptionMessage", "异常消息"),
                ("processStatus", "处理状态"),
                ("processTime", "处理时间"),
                ("createTime", "创建时间"),
            ],
        },
        "demo01_contact" => ExportSpec {
            table: "yudao_demo01_contact",
            filename: "infra-demo01-contact.xlsx",
            sheet_name: "联系人",
            columns: &[
                ("id", "编号"),
                ("name", "名字"),
                ("sex", "性别"),
                ("birthday", "生日"),
                ("description", "简介"),
                ("avatar", "头像"),
                ("createTime", "创建时间"),
            ],
        },
        "demo02_category" => ExportSpec {
            table: "yudao_demo02_category",
            filename: "infra-demo02-category.xlsx",
            sheet_name: "分类",
            columns: &[
                ("id", "编号"),
                ("name", "名字"),
                ("parentId", "父级编号"),
                ("createTime", "创建时间"),
            ],
        },
        "demo03_student" => ExportSpec {
            table: "yudao_demo03_student",
            filename: "infra-demo03-student.xlsx",
            sheet_name: "学生",
            columns: &[
                ("id", "编号"),
                ("name", "名字"),
                ("sex", "性别"),
                ("birthday", "生日"),
                ("description", "简介"),
                ("createTime", "创建时间"),
            ],
        },
        _ => return None,
    };
    Some(spec)
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
            .map(|value| value_string(Some(value)))
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => value.unwrap().to_string(),
    }
}
