use anyhow::Context;
use rust_toon_framework_security::CurrentUser;
use sqlx::{Postgres, Transaction};

pub async fn replace_posts(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: i64,
    post_ids: &[i64],
    operator: &str,
    tenant_id: i64,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE system_user_post
         SET deleted = 1, updater = $2, update_time = now()
         WHERE user_id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .bind(operator)
    .execute(&mut **transaction)
    .await
    .context("failed to clear user posts")?;

    for post_id in post_ids {
        sqlx::query(
            "INSERT INTO system_user_post
             (id, user_id, post_id, creator, create_time, updater, update_time, deleted, tenant_id)
             SELECT nextval('system_user_post_seq'), $1, post.id, $3, now(), $3, now(), 0, $4
             FROM system_post post
             WHERE post.id = $2 AND post.deleted = 0 AND post.tenant_id = $4",
        )
        .bind(user_id)
        .bind(post_id)
        .bind(operator)
        .bind(tenant_id)
        .execute(&mut **transaction)
        .await
        .context("failed to assign user post")?
        .rows_affected()
        .eq(&1)
        .then_some(())
        .context("post does not exist in the user's tenant")?;
    }
    Ok(())
}

/// 替换用户的角色集合。分配前先做角色挂载部门的双侧校验
/// （角色子树覆盖用户部门 ∧ 操作者管辖子树覆盖挂载节点）；
/// 目标部门在事务内读取，调用方先更新部门再替换角色即可校验到新值。
pub async fn replace_roles(
    transaction: &mut Transaction<'_, Postgres>,
    operator: Option<&CurrentUser>,
    user_id: i64,
    role_ids: &[i64],
    tenant_id: i64,
) -> anyhow::Result<()> {
    if !role_ids.is_empty() {
        let target_dept: Option<i64> = sqlx::query_scalar(
            "SELECT dept_id FROM system_users WHERE id = $1 AND deleted = 0 AND tenant_id = $2",
        )
        .bind(user_id)
        .bind(tenant_id)
        .fetch_optional(&mut **transaction)
        .await
        .context("failed to read user department")?
        .flatten();
        super::role_binding::validate_role_assignment(
            transaction,
            operator,
            target_dept,
            role_ids,
            tenant_id,
        )
        .await
        .map_err(|error| anyhow::anyhow!("{}", error.message()))?;
    }
    let operator_name = operator
        .map(|user| user.username.as_str())
        .unwrap_or("system");
    sqlx::query(
        "UPDATE system_user_role
         SET deleted = 1, updater = $2, update_time = now()
         WHERE user_id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .bind(operator_name)
    .execute(&mut **transaction)
    .await
    .context("failed to clear user roles")?;

    for role_id in role_ids {
        sqlx::query(
            "INSERT INTO system_user_role
             (id, user_id, role_id, creator, create_time, updater, update_time, deleted, tenant_id)
             SELECT nextval('system_user_role_seq'), $1, role.id, $3, now(), $3, now(), 0, $4
             FROM system_role role
             WHERE role.id = $2 AND role.deleted = 0 AND role.tenant_id = $4",
        )
        .bind(user_id)
        .bind(role_id)
        .bind(operator_name)
        .bind(tenant_id)
        .execute(&mut **transaction)
        .await
        .context("failed to assign user role")?
        .rows_affected()
        .eq(&1)
        .then_some(())
        .context("role does not exist in the user's tenant")?;
    }
    Ok(())
}

pub async fn delete_all(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: i64,
    operator: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "UPDATE system_user_post SET deleted = 1, updater = $2, update_time = now()
         WHERE user_id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .bind(operator)
    .execute(&mut **transaction)
    .await
    .context("failed to delete user posts")?;
    sqlx::query(
        "UPDATE system_user_role SET deleted = 1, updater = $2, update_time = now()
         WHERE user_id = $1 AND deleted = 0",
    )
    .bind(user_id)
    .bind(operator)
    .execute(&mut **transaction)
    .await
    .context("failed to delete user roles")?;
    Ok(())
}
