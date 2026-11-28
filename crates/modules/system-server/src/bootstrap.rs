use anyhow::{Context, bail};

use crate::SystemState;

pub async fn initialize(state: &SystemState) -> anyhow::Result<()> {
    ensure_identity_uuids(&state.pool).await?;
    remap_business_user_uuids(&state.pool).await?;

    let administrators = sqlx::query_scalar::<_, i64>(
        "SELECT count(*)
         FROM system_users u
         JOIN system_user_role ur ON ur.user_id = u.id AND ur.deleted = 0
         JOIN system_role r ON r.id = ur.role_id AND r.deleted = 0
         WHERE u.deleted = 0 AND u.status = 0 AND r.status = 0
           AND r.code = 'super_admin'",
    )
    .fetch_one(&state.pool)
    .await
    .context("failed to verify Yudao administrator")?;
    if administrators == 0 {
        bail!("Yudao system_users has no enabled super administrator");
    }
    tracing::info!(administrators, "Yudao administrators are ready");
    Ok(())
}

/// Backfill `system_users.identity_uuid` (SM3-derived, 国密) for any user the
/// baseline migration did not cover (runtime-created accounts on databases
/// migrated before 0033). New accounts write the column directly; this pass
/// only exists so no row can stay behind the unique-index invariant.
async fn ensure_identity_uuids(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let missing: Vec<i64> =
        sqlx::query_scalar("SELECT id FROM system_users WHERE identity_uuid IS NULL ORDER BY id")
            .fetch_all(pool)
            .await
            .context("failed to list users missing identity uuids")?;
    if missing.is_empty() {
        return Ok(());
    }
    let mut transaction = pool
        .begin()
        .await
        .context("failed to begin identity backfill")?;
    for id in missing {
        sqlx::query("UPDATE system_users SET identity_uuid = $2 WHERE id = $1")
            .bind(id)
            .bind(crate::infrastructure::identity_uuid_for(id))
            .execute(&mut *transaction)
            .await
            .context("failed to backfill identity uuid")?;
    }
    transaction
        .commit()
        .await
        .context("failed to commit identity backfill")?;
    tracing::info!("backfilled SM3 identity uuids for users lacking one");
    Ok(())
}

/// RustToon-specific: business tables store the API-facing user uuid as data
/// (asset/project ownership, AI chat ownership). Rows written during the
/// retired md5('yudao-user:') era must follow their owner to the SM3 identity
/// or ownership silently breaks after the cutover. Idempotent: once remapped,
/// rows no longer match the md5 derivation and later boots update nothing.
async fn remap_business_user_uuids(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    let statements: [(&str, &str); 6] = [
        (
            "media.assets.owner_user_id",
            "UPDATE media.assets a SET owner_user_id = u.identity_uuid \
             FROM system_users u \
             WHERE a.owner_user_id = md5('yudao-user:' || u.id::text)::uuid \
               AND u.identity_uuid IS NOT NULL AND u.deleted = 0",
        ),
        (
            "toon.projects.owner_user_id",
            "UPDATE toon.projects p SET owner_user_id = u.identity_uuid \
             FROM system_users u \
             WHERE p.owner_user_id = md5('yudao-user:' || u.id::text)::uuid \
               AND u.identity_uuid IS NOT NULL AND u.deleted = 0",
        ),
        (
            "toonflow.projects.user_id",
            "UPDATE toonflow.projects p SET user_id = u.identity_uuid \
             FROM system_users u \
             WHERE p.user_id = md5('yudao-user:' || u.id::text)::uuid \
               AND u.identity_uuid IS NOT NULL AND u.deleted = 0",
        ),
        (
            "ai.chat_roles.user_id",
            "UPDATE ai.chat_roles c SET user_id = u.identity_uuid::text \
             FROM system_users u \
             WHERE c.user_id = md5('yudao-user:' || u.id::text)::uuid::text \
               AND u.identity_uuid IS NOT NULL AND u.deleted = 0",
        ),
        (
            "ai.chat_conversations.user_id",
            "UPDATE ai.chat_conversations c SET user_id = u.identity_uuid::text \
             FROM system_users u \
             WHERE c.user_id = md5('yudao-user:' || u.id::text)::uuid::text \
               AND u.identity_uuid IS NOT NULL AND u.deleted = 0",
        ),
        (
            "ai.images.user_id",
            "UPDATE ai.images i SET user_id = u.identity_uuid::text \
             FROM system_users u \
             WHERE i.user_id = md5('yudao-user:' || u.id::text)::uuid::text \
               AND u.identity_uuid IS NOT NULL AND u.deleted = 0",
        ),
    ];
    for (column, statement) in statements {
        let remapped = sqlx::query(statement)
            .execute(pool)
            .await
            .with_context(|| format!("failed to remap {column} to identity uuids"))?
            .rows_affected();
        if remapped > 0 {
            tracing::info!(remapped, column, "remapped md5-era user uuids");
        }
    }
    Ok(())
}

#[cfg(test)]
mod identity_uuid_database_tests {
    use std::time::Duration;

    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
    use uuid::Uuid;

    use super::{ensure_identity_uuids, remap_business_user_uuids};
    use crate::infrastructure::identity_uuid_for;

    const TENANT: i64 = 970_300;

    /// 0033 国密身份闭环：运行期用户缺 identity_uuid 时由启动回填补齐，
    /// md5 时代的业务表 uuid 随属主一起重映射，且重映射幂等。
    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn backfills_identity_and_remaps_md5_era_business_rows() {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();

        cleanup(&pool).await;
        let user_id = TENANT + 1;
        // 模拟 0033 之前创建的运行期用户：identity_uuid 为 NULL。
        sqlx::query(
            "INSERT INTO system_users(id,username,password,nickname,status,tenant_id,dept_id)
             VALUES($1,'gm_identity_user','x','国密测试',0,$2,0)",
        )
        .bind(user_id)
        .bind(TENANT)
        .execute(&pool)
        .await
        .unwrap();

        ensure_identity_uuids(&pool).await.unwrap();
        let stored: Uuid =
            sqlx::query_scalar("SELECT identity_uuid FROM system_users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored, identity_uuid_for(user_id));
        // 幂等：再次回填不再改动。
        ensure_identity_uuids(&pool).await.unwrap();
        let stored_again: Option<Uuid> =
            sqlx::query_scalar("SELECT identity_uuid FROM system_users WHERE id = $1")
                .bind(user_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored_again, Some(stored));

        // md5 时代的业务行：uuid/text 列各取其一代表（media / toonflow / ai）。
        let md5_uuid: Uuid = sqlx::query_scalar("SELECT md5('yudao-user:' || $1::text)::uuid")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO media.assets(id, object_key, content_type, size_bytes, owner_user_id)
             VALUES($1,'gm/test.mp4','video/mp4',1,$2)",
        )
        .bind(Uuid::from_u128(0x0069_7003_0000_0001))
        .bind(md5_uuid)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO toon.projects(id, name, owner_user_id) VALUES($1,'国密测试',$2)")
            .bind(Uuid::from_u128(0x0069_7003_0000_0002))
            .bind(md5_uuid)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO toonflow.projects(id, name, user_id, create_time, update_time)
             VALUES($1,'国密测试',$2,1,1)",
        )
        .bind(0x0069_7003_0000_0003i64)
        .bind(md5_uuid)
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ai.model_configs(id, name, key, platform, type, model, create_time, update_time)
             VALUES(0x0069_7003_0000_0009,'国密模型','gm-key','openai','chat','gm-chat',1,1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO ai.chat_conversations(id, user_id, title, model_id, create_time, update_time)
             VALUES($1,$2,'国密测试',0x0069_7003_0000_0009,1,1)",
        )
        .bind(0x0069_7003_0000_0004i64)
        .bind(md5_uuid.to_string())
        .execute(&pool)
        .await
        .unwrap();

        remap_business_user_uuids(&pool).await.unwrap();
        let (asset_owner, project_owner, flow_user, chat_user): (Uuid, Uuid, Uuid, String) =
            sqlx::query_as(
                "SELECT (SELECT owner_user_id FROM media.assets WHERE id = $2),
                        (SELECT owner_user_id FROM toon.projects WHERE id = $3),
                        (SELECT user_id FROM toonflow.projects WHERE id = $4),
                        (SELECT user_id FROM ai.chat_conversations WHERE id = $5)",
            )
            .bind(user_id)
            .bind(Uuid::from_u128(0x0069_7003_0000_0001))
            .bind(Uuid::from_u128(0x0069_7003_0000_0002))
            .bind(0x0069_7003_0000_0003i64)
            .bind(0x0069_7003_0000_0004i64)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(asset_owner, stored);
        assert_eq!(project_owner, stored);
        assert_eq!(flow_user, stored);
        assert_eq!(chat_user, stored.to_string());

        // 幂等：已重映射的行不再命中 md5 条件，值保持不变。
        remap_business_user_uuids(&pool).await.unwrap();
        let still: Uuid =
            sqlx::query_scalar("SELECT owner_user_id FROM media.assets WHERE id = $1")
                .bind(Uuid::from_u128(0x0069_7003_0000_0001))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(still, stored);

        cleanup(&pool).await;
    }

    async fn cleanup(pool: &sqlx::PgPool) {
        sqlx::query("DELETE FROM media.assets WHERE id = $1")
            .bind(Uuid::from_u128(0x0069_7003_0000_0001))
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM toon.projects WHERE id = $1")
            .bind(Uuid::from_u128(0x0069_7003_0000_0002))
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM toonflow.projects WHERE id = $1")
            .bind(0x0069_7003_0000_0003i64)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai.chat_conversations WHERE id = $1")
            .bind(0x0069_7003_0000_0004i64)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM ai.model_configs WHERE id = $1")
            .bind(0x0069_7003_0000_0009i64)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM system_users WHERE id = $1")
            .bind(TENANT + 1)
            .execute(pool)
            .await
            .unwrap();
    }
}
