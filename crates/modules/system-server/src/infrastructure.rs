use anyhow::{Context, anyhow, ensure};
use chrono::{DateTime, Utc};
use rust_toon_framework_database::PgPool;
use rust_toon_framework_security::{CurrentUser, DataScope, Permission, PermissionSet};
use sqlx::FromRow;
use uuid::Uuid;

/// SM3-derived identity UUID for a numeric user id (国密 replacement for the
/// retired md5 mapping). Stored in `system_users.identity_uuid`; all API
/// surfaces present this UUID as the user id.
pub fn identity_uuid_for(user_id: i64) -> Uuid {
    Uuid::from_bytes(rust_toon_framework_gm::sm3_uuid_bytes(&format!(
        "{}{user_id}",
        rust_toon_framework_gm::IDENTITY_DOMAIN
    )))
}

#[derive(Debug, FromRow)]
pub struct UserAccount {
    pub id: Uuid,
    pub username: String,
    pub password_hash: String,
    pub tenant_id: Option<String>,
    pub status: String,
    pub locked_until: Option<DateTime<Utc>>,
}

pub async fn find_account_by_username(
    pool: &PgPool,
    username: &str,
    tenant_id: Option<i64>,
) -> anyhow::Result<Option<UserAccount>> {
    sqlx::query_as::<_, UserAccount>(
        "SELECT source.identity_uuid AS id,
                source.username, source.password AS password_hash,
                source.tenant_id::text AS tenant_id,
                CASE source.status WHEN 0 THEN 'active' ELSE 'disabled' END AS status,
                source.locked_until
         FROM system_users source
         WHERE lower(source.username) = lower($1)
           AND (
               source.tenant_id = $2
               OR (
                   $2::bigint IS NULL
                   AND 1 = (SELECT count(*) FROM system_users duplicate
                            WHERE lower(duplicate.username) = lower($1)
                              AND duplicate.deleted = 0)
               )
           )
           AND source.deleted = 0
         ORDER BY source.id
         LIMIT 1",
    )
    .bind(username)
    .bind(tenant_id)
    .fetch_optional(pool)
    .await
    .context("failed to query user account")
}

pub async fn find_account_by_id(
    pool: &PgPool,
    user_id: Uuid,
) -> anyhow::Result<Option<UserAccount>> {
    sqlx::query_as::<_, UserAccount>(
        "SELECT source.identity_uuid AS id,
                source.username, source.password AS password_hash,
                source.tenant_id::text AS tenant_id,
                CASE source.status WHEN 0 THEN 'active' ELSE 'disabled' END AS status,
                source.locked_until
         FROM system_users source
         WHERE source.identity_uuid = $1
           AND source.deleted = 0
         ORDER BY source.id
         LIMIT 1",
    )
    .bind(user_id)
    .fetch_optional(pool)
    .await
    .context("failed to query user account")
}

pub async fn record_failed_login(
    pool: &PgPool,
    user_id: Uuid,
    max_failed_attempts: i32,
    lock_duration_minutes: i32,
) -> anyhow::Result<()> {
    ensure!(
        max_failed_attempts > 0,
        "max failed attempts must be positive"
    );
    ensure!(
        lock_duration_minutes > 0,
        "lock duration minutes must be positive"
    );
    sqlx::query(
        "UPDATE system_users source
         SET failed_login_attempts = CASE
                 WHEN source.locked_until IS NOT NULL AND source.locked_until <= now()
                 THEN 1
                 ELSE LEAST(source.failed_login_attempts, $2 - 1) + 1
             END,
             locked_until = CASE
                 WHEN CASE
                          WHEN source.locked_until IS NOT NULL
                               AND source.locked_until <= now()
                          THEN 1
                          ELSE LEAST(source.failed_login_attempts, $2 - 1) + 1
                      END >= $2
                 THEN now() + make_interval(mins => $3)
                 ELSE NULL
             END,
             update_time = now()
         WHERE source.identity_uuid = $1
           AND source.deleted = 0
           AND (source.locked_until IS NULL OR source.locked_until <= now())",
    )
    .bind(user_id)
    .bind(max_failed_attempts)
    .bind(lock_duration_minutes)
    .execute(pool)
    .await
    .context("failed to record failed login")?;
    Ok(())
}

pub async fn record_successful_login(pool: &PgPool, user_id: Uuid) -> anyhow::Result<bool> {
    let result = sqlx::query(
        "UPDATE system_users source
         SET login_date = now(),
             failed_login_attempts = 0,
             locked_until = NULL,
             update_time = now()
         WHERE source.identity_uuid = $1
           AND source.deleted = 0
           AND (source.locked_until IS NULL OR source.locked_until <= now())",
    )
    .bind(user_id)
    .execute(pool)
    .await
    .context("failed to record successful login")?;
    Ok(result.rows_affected() == 1)
}

#[derive(Debug, FromRow)]
struct RolePermissionRow {
    role_code: String,
    data_scope: String,
    permission_code: Option<String>,
}

pub async fn load_current_user(
    pool: &PgPool,
    account: &UserAccount,
) -> anyhow::Result<CurrentUser> {
    let rows = sqlx::query_as::<_, RolePermissionRow>(
        "SELECT r.code AS role_code,
                CASE r.data_scope
                    WHEN 1 THEN 'all'
                    WHEN 2 THEN 'organization'
                    WHEN 3 THEN 'department'
                    WHEN 4 THEN 'department'
                    ELSE 'self_only'
                END AS data_scope,
                m.permission AS permission_code
         FROM system_users u
         JOIN system_tenant tenant
           ON tenant.id = u.tenant_id AND tenant.deleted = 0 AND tenant.status = 0
         LEFT JOIN system_tenant_package package
           ON package.id = tenant.package_id AND package.deleted = 0 AND package.status = 0
         JOIN system_user_role ur ON ur.user_id = u.id AND ur.deleted = 0
         JOIN system_role r ON r.id = ur.role_id AND r.deleted = 0 AND r.status = 0
         LEFT JOIN system_role_menu rm ON rm.role_id = r.id AND rm.deleted = 0
         LEFT JOIN system_menu m ON m.deleted = 0 AND m.status = 0
             AND m.permission <> ''
             AND (r.code = 'super_admin' OR m.id = rm.menu_id)
             AND (
                 tenant.package_id = 0
                 OR EXISTS (
                     SELECT 1
                     FROM jsonb_array_elements_text(package.menu_ids::jsonb) allowed(menu_id)
                     WHERE allowed.menu_id::bigint = m.id
                 )
             )
         WHERE u.identity_uuid = $1
           AND u.deleted = 0 AND u.status = 0
         ORDER BY r.code, m.permission",
    )
    .bind(account.id)
    .fetch_all(pool)
    .await
    .context("failed to load effective permissions")?;

    let mut roles = Vec::new();
    let mut permissions = Vec::new();
    let mut data_scope = DataScope::SelfOnly;
    for row in rows {
        if !roles.contains(&row.role_code) {
            roles.push(row.role_code);
        }
        data_scope = widest_scope(data_scope, parse_scope(&row.data_scope)?);
        if let Some(code) = row.permission_code {
            permissions.push(
                Permission::new(code)
                    .map_err(|error| anyhow!("invalid permission in database: {error}"))?,
            );
        }
    }
    Ok(CurrentUser {
        user_id: account.id.to_string(),
        username: account.username.clone(),
        tenant_id: account.tenant_id.clone(),
        role_codes: roles,
        permissions: PermissionSet::new(permissions),
        data_scope,
    })
}

fn parse_scope(scope: &str) -> anyhow::Result<DataScope> {
    match scope {
        "self_only" => Ok(DataScope::SelfOnly),
        "department" => Ok(DataScope::Department),
        "organization" => Ok(DataScope::Organization),
        "all" => Ok(DataScope::All),
        _ => Err(anyhow!("invalid data scope in database: {scope}")),
    }
}

fn widest_scope(left: DataScope, right: DataScope) -> DataScope {
    fn rank(scope: DataScope) -> u8 {
        match scope {
            DataScope::SelfOnly => 0,
            DataScope::Department => 1,
            DataScope::Organization => 2,
            DataScope::All => 3,
        }
    }

    if rank(left) >= rank(right) {
        left
    } else {
        right
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, Utc};
    use rust_toon_framework_security::DataScope;
    use sqlx::postgres::PgPoolOptions;
    use uuid::Uuid;

    use super::{identity_uuid_for, record_failed_login, record_successful_login, widest_scope};

    #[test]
    fn combines_multiple_roles_using_the_widest_data_scope() {
        assert_eq!(
            widest_scope(DataScope::Department, DataScope::Organization),
            DataScope::Organization
        );
    }

    #[tokio::test]
    async fn rejects_invalid_lockout_configuration_before_querying() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://rust_toon:rust_toon@127.0.0.1/rust_toon_test")
            .expect("valid lazy test database URL");
        let user_id = Uuid::nil();

        assert!(record_failed_login(&pool, user_id, 0, 15).await.is_err());
        assert!(record_failed_login(&pool, user_id, 5, 0).await.is_err());
    }

    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn login_lockout_database_tests() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .connect(&url)
            .await
            .expect("connect lockout test database");
        // Baseline admin (id=1): the migration 0033 literal must match the
        // Rust derivation.
        let user_id = identity_uuid_for(1);
        let stored: Option<Uuid> = sqlx::query_scalar(
            "SELECT identity_uuid
             FROM system_users
             WHERE id = 1 AND deleted = 0",
        )
        .fetch_one(&pool)
        .await
        .expect("baseline user");
        assert_eq!(stored, Some(user_id), "admin identity literal drifted");

        sqlx::query(
            "UPDATE system_users
             SET failed_login_attempts = 0, locked_until = NULL
             WHERE identity_uuid = $1",
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("reset baseline lockout state");

        let (one, two, three, four, five) = tokio::join!(
            record_failed_login(&pool, user_id, 5, 15),
            record_failed_login(&pool, user_id, 5, 15),
            record_failed_login(&pool, user_id, 5, 15),
            record_failed_login(&pool, user_id, 5, 15),
            record_failed_login(&pool, user_id, 5, 15),
        );
        for result in [one, two, three, four, five] {
            result.expect("record concurrent failed login");
        }

        let (attempts, locked_until): (i32, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT failed_login_attempts, locked_until
             FROM system_users
             WHERE identity_uuid = $1",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("read locked account");
        assert_eq!(attempts, 5);
        let locked_until = locked_until.expect("fifth failure must lock the account");
        assert!(locked_until > Utc::now());

        // A request that was already in flight when another request created
        // the lock must neither increment the counter nor extend its duration.
        record_failed_login(&pool, user_id, 5, 15)
            .await
            .expect("active lock is an idempotent no-op");
        let unchanged: (i32, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT failed_login_attempts, locked_until
             FROM system_users
             WHERE identity_uuid = $1",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("read unchanged active lock");
        assert_eq!(unchanged, (5, Some(locked_until)));
        assert!(
            !record_successful_login(&pool, user_id)
                .await
                .expect("active lock success check")
        );

        // Once the lock expires a fresh failure window starts at one instead
        // of immediately re-locking because of the previous window's count.
        sqlx::query(
            "UPDATE system_users
             SET locked_until = now() - interval '1 second'
             WHERE identity_uuid = $1",
        )
        .bind(user_id)
        .execute(&pool)
        .await
        .expect("expire account lock");
        record_failed_login(&pool, user_id, 5, 15)
            .await
            .expect("start fresh failure window");
        let fresh_window: (i32, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT failed_login_attempts, locked_until
             FROM system_users
             WHERE identity_uuid = $1",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("read fresh failure window");
        assert_eq!(fresh_window, (1, None));

        assert!(
            record_successful_login(&pool, user_id)
                .await
                .expect("record successful login")
        );
        let reset: (i32, Option<DateTime<Utc>>) = sqlx::query_as(
            "SELECT failed_login_attempts, locked_until
             FROM system_users
             WHERE identity_uuid = $1",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .expect("read reset login state");
        assert_eq!(reset, (0, None));
    }
}
