//! 角色挂载部门（`system_role.dept_id`）的分配规则（自 rustset 同步）。
//!
//! - `dept_id = 0`：公司级角色（虚拟租户根）。SaaS 多租户下部门树形态不可控
//!   （可能为空或多顶级森林），真实根节点表达不了"覆盖全租户"，因此公司级
//!   用虚拟根 0 表示：可分配给租户内任何用户，但只有公司级管理员能发放。
//! - `dept_id ≠ 0`：挂载到部门树节点，受两条规则约束：
//!   1. 角色侧：只能分配给「挂载节点子树内」的用户（挂在上级的角色可以发给下级用户）；
//!   2. 操作者侧：非公司级管理员只能发放「挂载节点落在自己部门子树内」的角色，
//!      发放权不逆向流——下级管理员不能把上级挂载或公司级的角色发出去。
//!
//! 公司级 = `super_admin` 或任一角色 `data_scope = 1`（全部数据权限）。
//! 数据权限（data_scope 1-5）语义不变，仍相对用户自己的部门解析。

use std::collections::HashSet;

use rust_toon_framework_security::{CurrentUser, DataScope};
use rust_toon_framework_web::AppError;

/// 部门树递归深度上限（防异常成环，正常 ≤3 级）。
const DEPT_TREE_DEPTH_LIMIT: i32 = 8;

pub fn is_company_level(user: &CurrentUser) -> bool {
    user.role_codes.iter().any(|code| code == "super_admin") || user.data_scope == DataScope::All
}

/// 节点子树 id 集合（含自身）：向下。
pub(crate) async fn dept_subtree_ids(
    conn: &mut sqlx::PgConnection,
    dept_id: i64,
    tenant_id: i64,
) -> Result<HashSet<i64>, AppError> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE subtree AS (
             SELECT d.id, 0 AS depth FROM system_dept d
             WHERE d.id = $1 AND d.deleted = 0 AND d.tenant_id = $2
             UNION ALL
             SELECT d.id, s.depth + 1 FROM system_dept d
             JOIN subtree s ON d.parent_id = s.id
             WHERE d.deleted = 0 AND d.tenant_id = $2 AND s.depth < $3
         )
         SELECT id FROM subtree",
    )
    .bind(dept_id)
    .bind(tenant_id)
    .bind(DEPT_TREE_DEPTH_LIMIT)
    .fetch_all(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to read dept subtree"))?;
    Ok(ids.into_iter().collect())
}

/// 节点自身 + 祖先链：判断「挂载节点是否覆盖该部门」用（节点覆盖自身及其后代，
/// 等价于挂载节点 ∈ 该部门的自身+祖先链）。
async fn dept_self_or_ancestors(
    conn: &mut sqlx::PgConnection,
    dept_id: i64,
    tenant_id: i64,
) -> Result<HashSet<i64>, AppError> {
    let ids: Vec<i64> = sqlx::query_scalar(
        "WITH RECURSIVE up AS (
             SELECT d.id, d.parent_id, 0 AS depth FROM system_dept d
             WHERE d.id = $1 AND d.deleted = 0 AND d.tenant_id = $2
             UNION ALL
             SELECT d.id, d.parent_id, up.depth + 1 FROM system_dept d
             JOIN up ON d.id = up.parent_id
             WHERE d.deleted = 0 AND d.tenant_id = $2 AND up.depth < $3
         )
         SELECT id FROM up",
    )
    .bind(dept_id)
    .bind(tenant_id)
    .bind(DEPT_TREE_DEPTH_LIMIT)
    .fetch_all(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to read dept ancestors"))?;
    Ok(ids.into_iter().collect())
}

/// 操作者（登录态）的归属部门。查不到（无部门/已删除）返回 None。
/// CurrentUser.user_id 是 `system_users.identity_uuid` 的 UUID 形式
/// （SM3 派生，与 authorized_menus 的映射一致）。
async fn operator_dept_id(
    conn: &mut sqlx::PgConnection,
    operator: &CurrentUser,
    tenant_id: i64,
) -> Result<Option<i64>, AppError> {
    let identity_uuid = uuid::Uuid::parse_str(&operator.user_id)
        .map_err(|_| AppError::bad_request("invalid operator identity"))?;
    sqlx::query_scalar::<_, Option<i64>>(
        "SELECT dept_id FROM system_users
         WHERE identity_uuid = $1
           AND deleted = 0 AND tenant_id = $2",
    )
    .bind(identity_uuid)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to read operator department"))
    .map(|row| row.flatten())
}

/// 非公司级操作者的管辖子树；公司级返回 None（不限制）。
async fn operator_scope(
    conn: &mut sqlx::PgConnection,
    operator: Option<&CurrentUser>,
    tenant_id: i64,
) -> Result<Option<HashSet<i64>>, AppError> {
    let Some(operator) = operator else {
        return Ok(None);
    };
    if is_company_level(operator) {
        return Ok(None);
    }
    let scope = match operator_dept_id(conn, operator, tenant_id).await? {
        Some(dept_id) => dept_subtree_ids(conn, dept_id, tenant_id).await?,
        None => HashSet::new(),
    };
    Ok(Some(scope))
}

struct MountedRole {
    name: String,
    dept_id: i64,
}

/// 拉取这批角色（含公司级 dept_id=0）。不存在的角色由 `replace_roles`
/// 的插入校验兜底，这里只管挂载规则。
async fn bound_roles(
    conn: &mut sqlx::PgConnection,
    role_ids: &[i64],
    tenant_id: i64,
) -> Result<Vec<MountedRole>, AppError> {
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT name, dept_id FROM system_role
         WHERE deleted = 0 AND tenant_id = $1 AND id = ANY($2)",
    )
    .bind(tenant_id)
    .bind(role_ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to read role mounts"))?;
    Ok(rows
        .into_iter()
        .map(|(name, dept_id)| MountedRole { name, dept_id })
        .collect())
}

/// 分配角色前的双侧校验（规则一 + 规则二）。`target_dept` 是目标用户
/// 【分配后】的部门（调用方在事务里先更新部门再校验即可拿到新值）。
pub async fn validate_role_assignment(
    conn: &mut sqlx::PgConnection,
    operator: Option<&CurrentUser>,
    target_dept: Option<i64>,
    role_ids: &[i64],
    tenant_id: i64,
) -> Result<(), AppError> {
    if role_ids.is_empty() {
        return Ok(());
    }
    // 去重，保序
    let mut unique: Vec<i64> = Vec::with_capacity(role_ids.len());
    for id in role_ids {
        if !unique.contains(id) {
            unique.push(*id);
        }
    }
    let roles = bound_roles(conn, &unique, tenant_id).await?;
    if roles.is_empty() {
        return Ok(());
    }

    // 规则一：目标用户的部门必须被角色挂载节点覆盖（挂载节点 ∈ 部门自身+祖先链）。
    // 公司级（dept_id=0）覆盖全租户，天然通过。
    let mounted: Vec<&MountedRole> = roles.iter().filter(|role| role.dept_id != 0).collect();
    let target_chain = match target_dept {
        Some(dept_id) => dept_self_or_ancestors(conn, dept_id, tenant_id).await?,
        // 无部门的用户不能被分配挂载角色
        None => HashSet::new(),
    };
    let out_of_reach: Vec<&str> = mounted
        .iter()
        .filter(|role| !target_chain.contains(&role.dept_id))
        .map(|role| role.name.as_str())
        .collect();
    if !out_of_reach.is_empty() {
        return Err(AppError::bad_request(format!(
            "角色「{}」挂载的部门不覆盖该用户所在部门，无法分配",
            out_of_reach.join("、")
        )));
    }

    // 规则二：非公司级管理员只能发放自己管辖子树内挂载的角色；
    // 公司级角色（dept_id=0）是虚拟租户根，不在任何子树内——只有公司级管理员能发。
    let scope = operator_scope(conn, operator, tenant_id).await?;
    if let Some(scope) = scope {
        let beyond: Vec<&str> = roles
            .iter()
            .filter(|role| !scope.contains(&role.dept_id))
            .map(|role| role.name.as_str())
            .collect();
        if !beyond.is_empty() {
            return Err(AppError::bad_request(format!(
                "角色「{}」为上级/公司级角色，超出您的管辖范围，无法由您分配",
                beyond.join("、")
            )));
        }
    }
    Ok(())
}

/// 角色创建/改挂载时的挂载点校验：节点存在，且在操作者管辖子树内。
pub async fn validate_role_mount(
    conn: &mut sqlx::PgConnection,
    operator: Option<&CurrentUser>,
    dept_id: i64,
    tenant_id: i64,
) -> Result<(), AppError> {
    if dept_id == 0 {
        return Ok(()); // 0 = 全局（未挂载）
    }
    let exists = sqlx::query_scalar::<_, i64>(
        "SELECT count(*) FROM system_dept WHERE id = $1 AND deleted = 0 AND tenant_id = $2",
    )
    .bind(dept_id)
    .bind(tenant_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to read dept"))?;
    if exists == 0 {
        return Err(AppError::bad_request("挂载部门不存在"));
    }
    let scope = operator_scope(conn, operator, tenant_id).await?;
    if let Some(scope) = scope
        && !scope.contains(&dept_id)
    {
        return Err(AppError::bad_request(
            "不能在您的管辖范围之外的部门挂载角色",
        ));
    }
    Ok(())
}

/// 用户调动部门时的冲突检查：现有挂载角色必须仍覆盖新部门，
/// 否则阻止调动（明示冲突角色，不静默摘除）。
pub async fn ensure_roles_cover_dept(
    conn: &mut sqlx::PgConnection,
    user_id: i64,
    new_dept: Option<i64>,
    tenant_id: i64,
) -> Result<(), AppError> {
    let chain = match new_dept {
        Some(dept_id) => dept_self_or_ancestors(conn, dept_id, tenant_id).await?,
        None => HashSet::new(),
    };
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT r.name, r.dept_id
         FROM system_user_role ur
         JOIN system_role r
           ON r.id = ur.role_id AND r.deleted = 0 AND r.tenant_id = $2
         WHERE ur.user_id = $1 AND ur.deleted = 0 AND r.dept_id <> 0",
    )
    .bind(user_id)
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to read user roles"))?;
    let conflicts: Vec<String> = rows
        .into_iter()
        .filter(|(_, dept_id)| !chain.contains(dept_id))
        .map(|(name, _)| name)
        .collect();
    if !conflicts.is_empty() {
        return Err(AppError::bad_request(format!(
            "调动后部门不在角色「{}」的挂载子树内，请先调整这些角色的分配",
            conflicts.join("、")
        )));
    }
    Ok(())
}

/// 角色改挂载时的持有者冲突检查：现有分配者的部门必须都落在新挂载子树内。
pub async fn ensure_holders_within_mount(
    conn: &mut sqlx::PgConnection,
    role_id: i64,
    new_dept_id: i64,
    tenant_id: i64,
) -> Result<(), AppError> {
    let subtree = dept_subtree_ids(conn, new_dept_id, tenant_id).await?;
    let depts: Vec<Option<i64>> = sqlx::query_scalar(
        "SELECT DISTINCT u.dept_id
         FROM system_user_role ur
         JOIN system_users u ON u.id = ur.user_id AND u.deleted = 0
         WHERE ur.role_id = $1 AND ur.deleted = 0 AND ur.tenant_id = $2",
    )
    .bind(role_id)
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to read role holders"))?;
    let outside = depts
        .into_iter()
        .filter(|dept| !dept.is_some_and(|dept| subtree.contains(&dept)))
        .count();
    if outside > 0 {
        return Err(AppError::bad_request(format!(
            "有 {outside} 个已分配该角色的用户不在新挂载部门的子树内，请先调整他们的角色"
        )));
    }
    Ok(())
}

/// 可分配给目标部门的角色 id 集合（规则一 ∧ 规则二；公司级角色仅对
/// 公司级操作者可见可发），并集上 `held_user_id` 当前已持有的角色
/// （避免编辑视图里出现幽灵 id）。供角色选择下拉过滤用。
pub async fn assignable_role_ids(
    conn: &mut sqlx::PgConnection,
    operator: &CurrentUser,
    target_dept: Option<i64>,
    held_user_id: Option<i64>,
    tenant_id: i64,
) -> Result<HashSet<i64>, AppError> {
    let rows = sqlx::query_as::<_, (i64, i64)>(
        "SELECT id, dept_id FROM system_role WHERE deleted = 0 AND tenant_id = $1",
    )
    .bind(tenant_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(|_| AppError::internal("failed to list roles"))?;
    let chain = match target_dept {
        Some(dept_id) => dept_self_or_ancestors(conn, dept_id, tenant_id).await?,
        None => HashSet::new(),
    };
    let scope = operator_scope(conn, Some(operator), tenant_id).await?;
    let mut allowed: HashSet<i64> = rows
        .into_iter()
        .filter(|(_, dept_id)| {
            // 公司级（0）：覆盖目标部门，但只有公司级操作者（scope=None）能发放；
            // 挂载角色：须覆盖目标部门 且 落在操作者管辖子树内。
            scope.is_none() && *dept_id == 0
                || (chain.contains(dept_id) && scope.as_ref().is_none_or(|s| s.contains(dept_id)))
        })
        .map(|(id, _)| id)
        .collect();
    if let Some(held_user_id) = held_user_id {
        let held: Vec<i64> = sqlx::query_scalar(
            "SELECT role_id FROM system_user_role WHERE user_id = $1 AND deleted = 0",
        )
        .bind(held_user_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(|_| AppError::internal("failed to read held roles"))?;
        allowed.extend(held);
    }
    Ok(allowed)
}

#[cfg(test)]
mod role_mount_database_tests {
    use std::time::Duration;

    use rust_toon_framework_database::{DatabaseConfig, connect, migrate};
    use rust_toon_framework_security::{CurrentUser, DataScope, PermissionSet};

    use super::{
        assignable_role_ids, ensure_holders_within_mount, ensure_roles_cover_dept,
        validate_role_assignment, validate_role_mount,
    };

    const DEPTH_LIMIT_NOTE: &str = "部门树：root → {A, B}，A → {A1}";

    /// 每个测试独立的 ID 段：用例在同一二进制内并行运行，共享固定 ID 会互相删除。
    struct Fixture {
        tenant: i64,
        pool: sqlx::PgPool,
        company_operator: CurrentUser,
        dept_operator: CurrentUser,
        root_dept: i64,
        dept_a: i64,
        dept_b: i64,
        dept_a1: i64,
        role_global: i64,
        role_a: i64,
        role_b: i64,
        target_a: i64,
        target_a1: i64,
        target_b: i64,
    }

    fn current_user(user_id: &str, company_level: bool) -> CurrentUser {
        CurrentUser {
            user_id: user_id.to_string(),
            username: "operator".into(),
            tenant_id: None,
            role_codes: if company_level {
                vec!["super_admin".into()]
            } else {
                vec![]
            },
            permissions: PermissionSet::default(),
            data_scope: if company_level {
                DataScope::All
            } else {
                DataScope::SelfOnly
            },
        }
    }

    /// 操作者的 CurrentUser.user_id 必须是 system_users 行的
    /// identity_uuid（SM3 派生）—— 与 authorized_menus 的映射一致。
    fn operator_token(user_row: i64) -> String {
        crate::infrastructure::identity_uuid_for(user_row).to_string()
    }

    impl Fixture {
        async fn dept_of(&self, user_row: i64) -> Option<i64> {
            sqlx::query_scalar("SELECT dept_id FROM system_users WHERE id = $1")
                .bind(user_row)
                .fetch_optional(&self.pool)
                .await
                .unwrap()
                .flatten()
        }
    }

    async fn cleanup(pool: &sqlx::PgPool, tenant: i64) {
        sqlx::query("DELETE FROM system_user_role WHERE tenant_id=$1")
            .bind(tenant)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM system_role WHERE tenant_id=$1")
            .bind(tenant)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM system_users WHERE tenant_id=$1")
            .bind(tenant)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM system_dept WHERE tenant_id=$1")
            .bind(tenant)
            .execute(pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM public.system_tenant WHERE id=$1")
            .bind(tenant)
            .execute(pool)
            .await
            .unwrap();
    }

    async fn setup(tenant: i64) -> Fixture {
        let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
        let config = DatabaseConfig::new(url, 1, 5, Duration::from_secs(10)).unwrap();
        let pool = connect(&config).await.unwrap();
        migrate(&pool).await.unwrap();
        cleanup(&pool, tenant).await;

        sqlx::query("INSERT INTO public.system_tenant(id,name,contact_name,status,package_id,account_count,expire_time) VALUES($1,'租户','联系人',0,0,100,'2099-12-31'::timestamp)")
            .bind(tenant).execute(&pool).await.unwrap();

        // 部门树（见 DEPTH_LIMIT_NOTE）：root → {A, B}，A → {A1}。
        let root = tenant + 1;
        let dept_a = tenant + 2;
        let dept_b = tenant + 3;
        let dept_a1 = tenant + 4;
        for (id, name, parent) in [
            (root, "根部门", 0i64),
            (dept_a, "A 部门", root),
            (dept_b, "B 部门", root),
            (dept_a1, "A1 小组", dept_a),
        ] {
            sqlx::query("INSERT INTO public.system_dept(id,name,parent_id,sort,status,tenant_id) VALUES($1,$2,$3,0,0,$4)")
                .bind(id).bind(name).bind(parent).bind(tenant).execute(&pool).await.unwrap();
        }

        // 角色：公司级（dept_id=0）/ 挂 A / 挂 B。
        let role_global = tenant + 11;
        let role_a = tenant + 12;
        let role_b = tenant + 13;
        for (id, dept) in [(role_global, 0i64), (role_a, dept_a), (role_b, dept_b)] {
            sqlx::query("INSERT INTO public.system_role(id,name,code,sort,data_scope,data_scope_dept_ids,status,type,tenant_id,dept_id) VALUES($1,$2,$3,0,1,'',0,2,$4,$5)")
                .bind(id)
                .bind(format!("角色{id}"))
                .bind(format!("role_{id}"))
                .bind(tenant)
                .bind(dept)
                .execute(&pool)
                .await
                .unwrap();
        }

        // 用户：公司级操作者（挂根）、A 部门操作者、目标用户（A/A1/B）。
        let operator_all = tenant + 21;
        let operator_a = tenant + 22;
        let target_a = tenant + 23;
        let target_a1 = tenant + 24;
        let target_b = tenant + 25;
        for (id, dept, username) in [
            (operator_all, root, "company_op"),
            (operator_a, dept_a, "a_op"),
            (target_a, dept_a, "target_a"),
            (target_a1, dept_a1, "target_a1"),
            (target_b, dept_b, "target_b"),
        ] {
            sqlx::query("INSERT INTO public.system_users(id,username,password,nickname,dept_id,status,tenant_id,identity_uuid) VALUES($1,$2,'x',$2,$3,0,$4,$5)")
                .bind(id).bind(username).bind(dept).bind(tenant)
                .bind(crate::infrastructure::identity_uuid_for(id))
                .execute(&pool).await.unwrap();
        }

        Fixture {
            tenant,
            company_operator: current_user(&operator_token(operator_all), true),
            dept_operator: current_user(&operator_token(operator_a), false),
            root_dept: root,
            dept_a,
            dept_b,
            dept_a1,
            role_global,
            role_a,
            role_b,
            target_a,
            target_a1,
            target_b,
            pool,
        }
    }

    /// 规则一：角色只能分配给挂载节点子树内的用户（公司级操作者也不例外）。
    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn rule_one_rejects_roles_outside_the_target_dept_subtree() {
        let f = setup(9_700_000).await;
        // A 角色发给 A 子树内用户（含孙级）→ 通过；发给 B 部门用户 → 拒绝。
        let mut conn = f.pool.acquire().await.unwrap();
        validate_role_assignment(
            &mut conn,
            Some(&f.company_operator),
            f.dept_of(f.target_a).await,
            &[f.role_a],
            f.tenant,
        )
        .await
        .unwrap();
        validate_role_assignment(
            &mut conn,
            Some(&f.company_operator),
            f.dept_of(f.target_a1).await,
            &[f.role_a],
            f.tenant,
        )
        .await
        .unwrap();
        let error = validate_role_assignment(
            &mut conn,
            Some(&f.company_operator),
            f.dept_of(f.target_b).await,
            &[f.role_a],
            f.tenant,
        )
        .await
        .unwrap_err();
        assert!(error.message().contains("不覆盖该用户所在部门"));
        // 公司级角色（0）覆盖全租户，任何用户都可分配。
        validate_role_assignment(
            &mut conn,
            Some(&f.company_operator),
            f.dept_of(f.target_b).await,
            &[f.role_global],
            f.tenant,
        )
        .await
        .unwrap();
        // f.dept_a 消费点：A 角色的挂载部门就是它。
        assert_eq!(
            sqlx::query_scalar::<_, i64>("SELECT dept_id FROM system_role WHERE id = $1")
                .bind(f.role_a)
                .fetch_one(&f.pool)
                .await
                .unwrap(),
            f.dept_a
        );
        cleanup(&f.pool, f.tenant).await;
    }

    /// 规则二：非公司级操作者只能发放自己管辖子树内挂载的角色；
    /// 公司级角色只有公司级管理员能发放。
    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn rule_two_bounds_what_dept_level_operators_can_grant() {
        let f = setup(9_710_000).await;
        // A 部门操作者：A 角色（挂载在管辖子树内）可发。
        let mut conn = f.pool.acquire().await.unwrap();
        validate_role_assignment(
            &mut conn,
            Some(&f.dept_operator),
            f.dept_of(f.target_a).await,
            &[f.role_a],
            f.tenant,
        )
        .await
        .unwrap();
        // B 角色（挂载 B，不覆盖 A 部门的目标）→ 规则一先拒绝。
        let error = validate_role_assignment(
            &mut conn,
            Some(&f.dept_operator),
            f.dept_of(f.target_a).await,
            &[f.role_b],
            f.tenant,
        )
        .await
        .unwrap_err();
        assert!(error.message().contains("不覆盖该用户所在部门"));
        // 公司级角色（虚拟根，不在任何真实子树内）→ 规则二拒绝。
        let error = validate_role_assignment(
            &mut conn,
            Some(&f.dept_operator),
            f.dept_of(f.target_a).await,
            &[f.role_global],
            f.tenant,
        )
        .await
        .unwrap_err();
        assert!(error.message().contains("超出您的管辖范围"));
        cleanup(&f.pool, f.tenant).await;
    }

    /// 调动部门与改挂载点的冲突检查 + 下拉过滤。
    #[tokio::test]
    #[ignore = "run with script/test-database-migrations.sh"]
    async fn move_conflicts_and_assignable_filtering() {
        let f = setup(9_720_000).await;
        sqlx::query("INSERT INTO public.system_user_role(id,user_id,role_id,tenant_id) VALUES(nextval('public.system_user_role_seq'),$1,$2,$3)")
            .bind(f.target_a).bind(f.role_a).bind(f.tenant).execute(&f.pool).await.unwrap();

        // 调到 B 部门：A 角色不覆盖 B → 拒绝；调到 A 的子组 → 通过。
        let mut conn = f.pool.acquire().await.unwrap();
        let error = ensure_roles_cover_dept(&mut conn, f.target_a, Some(f.dept_b), f.tenant)
            .await
            .unwrap_err();
        assert!(error.message().contains("不在角色"));
        ensure_roles_cover_dept(&mut conn, f.target_a, Some(f.dept_a1), f.tenant)
            .await
            .unwrap();

        // 角色改挂载：挂到 B（持有者 target_a 在 A）→ 拒绝；挂到根（覆盖 A）→ 通过。
        let error = ensure_holders_within_mount(&mut conn, f.role_a, f.dept_b, f.tenant)
            .await
            .unwrap_err();
        assert!(error.message().contains("不在新挂载部门的子树内"));
        ensure_holders_within_mount(&mut conn, f.role_a, f.root_dept, f.tenant)
            .await
            .unwrap();

        // 挂载点校验：不存在的部门拒绝。
        let error = validate_role_mount(&mut conn, None, f.tenant + 999, f.tenant)
            .await
            .unwrap_err();
        assert!(error.message().contains("挂载部门不存在"));

        // 下拉过滤（公司级操作者，目标部门 B）：全局 + 挂 B 角色可见，挂 A 角色不可见。
        let allowed = assignable_role_ids(
            &mut conn,
            &f.company_operator,
            Some(f.dept_b),
            None,
            f.tenant,
        )
        .await
        .unwrap();
        assert!(allowed.contains(&f.role_global));
        assert!(allowed.contains(&f.role_b));
        assert!(!allowed.contains(&f.role_a));

        cleanup(&f.pool, f.tenant).await;
    }

    // DEPTH_LIMIT_NOTE 提示树形；防止未使用告警。
    #[test]
    fn depth_note_is_documented() {
        assert!(!DEPTH_LIMIT_NOTE.is_empty());
    }
}
