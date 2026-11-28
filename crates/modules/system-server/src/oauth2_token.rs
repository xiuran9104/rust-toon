use anyhow::Context;
use chrono::{Duration, Utc};
use rust_toon_framework_database::PgPool;
use sqlx::FromRow;
use uuid::Uuid;

const REFRESH_TOKEN_TTL_DAYS: i64 = 1;
const CLIENT_ID: &str = "default";
const ADMIN_USER_TYPE: i16 = 2;

#[derive(Debug)]
pub enum TokenError {
    Invalid,
    Internal,
}

#[derive(Debug, FromRow)]
struct RefreshTokenRow {
    user_id: Uuid,
}

pub async fn create_token_pair(
    pool: &PgPool,
    user_id: Uuid,
    access_token: &str,
    access_token_ttl_seconds: u64,
) -> anyhow::Result<String> {
    let refresh_token = new_refresh_token();
    let access_expires_at = Utc::now() + Duration::seconds(access_token_ttl_seconds as i64);
    let refresh_expires_at = Utc::now() + Duration::days(REFRESH_TOKEN_TTL_DAYS);
    let mut transaction = pool
        .begin()
        .await
        .context("failed to begin token transaction")?;

    let account = sqlx::query_as::<_, (i64, String, i64)>(
        "SELECT id, username, tenant_id
         FROM system_users
         WHERE identity_uuid = $1
           AND deleted = 0",
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    .context("failed to resolve token user")?
    .context("token user does not exist")?;

    sqlx::query(
        "INSERT INTO system_oauth2_refresh_token
         (id, user_id, refresh_token, user_type, client_id, scopes, expires_time,
          creator, create_time, updater, update_time, deleted, tenant_id)
         VALUES (nextval('system_oauth2_refresh_token_seq'), $1, $2, $3, $4, NULL, $5,
                 $6, now(), $6, now(), 0, $7)",
    )
    .bind(account.0)
    .bind(&refresh_token)
    .bind(ADMIN_USER_TYPE)
    .bind(CLIENT_ID)
    .bind(refresh_expires_at.naive_utc())
    .bind(&account.1)
    .bind(account.2)
    .execute(&mut *transaction)
    .await
    .context("failed to create OAuth2 refresh token")?;

    sqlx::query(
        "INSERT INTO system_oauth2_access_token
         (id, user_id, user_type, user_info, access_token, refresh_token, client_id,
          scopes, expires_time, creator, create_time, updater, update_time, deleted, tenant_id)
         VALUES (nextval('system_oauth2_access_token_seq'), $1, $2, '{}', $3, $4, $5,
                 NULL, $6, $7, now(), $7, now(), 0, $8)",
    )
    .bind(account.0)
    .bind(ADMIN_USER_TYPE)
    .bind(access_token)
    .bind(&refresh_token)
    .bind(CLIENT_ID)
    .bind(access_expires_at.naive_utc())
    .bind(&account.1)
    .bind(account.2)
    .execute(&mut *transaction)
    .await
    .context("failed to create OAuth2 access token")?;

    transaction
        .commit()
        .await
        .context("failed to commit token pair")?;
    Ok(refresh_token)
}

pub async fn verify_refresh_token(pool: &PgPool, token: &str) -> Result<Uuid, TokenError> {
    sqlx::query_as::<_, RefreshTokenRow>(
        "SELECT users.identity_uuid AS user_id
         FROM system_oauth2_refresh_token token
         JOIN system_users users ON users.id = token.user_id
         WHERE token.refresh_token = $1
           AND token.user_type = $2
           AND token.client_id = $3
           AND token.deleted = 0
           AND token.expires_time > now()
           AND users.deleted = 0",
    )
    .bind(token)
    .bind(ADMIN_USER_TYPE)
    .bind(CLIENT_ID)
    .fetch_optional(pool)
    .await
    .map_err(|_| TokenError::Internal)?
    .map(|row| row.user_id)
    .ok_or(TokenError::Invalid)
}

pub async fn revoke_refresh_token(pool: &PgPool, token: &str) -> Result<Uuid, TokenError> {
    let user_id = verify_refresh_token(pool, token).await?;
    let mut transaction = pool.begin().await.map_err(|_| TokenError::Internal)?;
    sqlx::query(
        "UPDATE system_oauth2_access_token
         SET deleted = 1, update_time = now()
         WHERE refresh_token = $1 AND deleted = 0",
    )
    .bind(token)
    .execute(&mut *transaction)
    .await
    .map_err(|_| TokenError::Internal)?;
    sqlx::query(
        "UPDATE system_oauth2_refresh_token
         SET deleted = 1, update_time = now()
         WHERE refresh_token = $1 AND deleted = 0",
    )
    .bind(token)
    .execute(&mut *transaction)
    .await
    .map_err(|_| TokenError::Internal)?;
    transaction
        .commit()
        .await
        .map_err(|_| TokenError::Internal)?;
    Ok(user_id)
}

pub async fn revoke_user_tokens(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE system_oauth2_access_token
         SET deleted = 1, update_time = now()
         WHERE user_id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .execute(&mut **transaction)
    .await
    .context("failed to revoke user access tokens")?;
    sqlx::query(
        "UPDATE system_oauth2_refresh_token
         SET deleted = 1, update_time = now()
         WHERE user_id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .execute(&mut **transaction)
    .await
    .context("failed to revoke user refresh tokens")?;
    Ok(())
}

fn new_refresh_token() -> String {
    Uuid::new_v4().simple().to_string()
}
