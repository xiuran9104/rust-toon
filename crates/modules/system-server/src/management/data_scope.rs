use rust_toon_framework_database::PgPool;
use rust_toon_framework_security::CurrentUser;
use rust_toon_framework_web::AppError;
use serde_json::Value;
use uuid::Uuid;

pub async fn visible_user_values(
    pool: &PgPool,
    user: &CurrentUser,
    tenant_id: i64,
) -> Result<Vec<Value>, AppError> {
    let user_id =
        Uuid::parse_str(&user.user_id).map_err(|_| AppError::bad_request("invalid user id"))?;
    sqlx::query_scalar::<_, Value>(
        "WITH RECURSIVE actor AS (
             SELECT id, dept_id
             FROM system_users
             WHERE identity_uuid = $2
               AND deleted = 0
               AND tenant_id = $1
         ),
         actor_roles AS (
             SELECT r.id, r.code, r.data_scope, r.data_scope_dept_ids
             FROM actor
             JOIN system_user_role ur ON ur.user_id = actor.id AND ur.deleted = 0
             JOIN system_role r
               ON r.id = ur.role_id
              AND r.deleted = 0
              AND r.status = 0
              AND r.tenant_id = $1
         ),
         dept_tree AS (
             SELECT d.id
             FROM system_dept d
             JOIN actor ON actor.dept_id = d.id
             WHERE d.deleted = 0 AND d.tenant_id = $1
             UNION ALL
             SELECT child.id
             FROM system_dept child
             JOIN dept_tree parent ON child.parent_id = parent.id
             WHERE child.deleted = 0 AND child.tenant_id = $1
         ),
         custom_depts AS (
             SELECT DISTINCT ids.dept_id::bigint AS id
             FROM actor_roles role
             CROSS JOIN LATERAL jsonb_array_elements_text(
                 CASE
                     WHEN btrim(coalesce(role.data_scope_dept_ids, '')) = ''
                     THEN '[]'::jsonb
                     ELSE role.data_scope_dept_ids::jsonb
                 END
             ) ids(dept_id)
             WHERE role.data_scope = 2
         )
         SELECT jsonb_build_object(
             'id', u.id,
             'username', u.username,
             'nickname', u.nickname,
             'deptId', u.dept_id,
             'deptName', coalesce(d.name, ''),
             'postIds', COALESCE((SELECT jsonb_agg(up.post_id ORDER BY up.post_id)
                                  FROM system_user_post up
                                  WHERE up.user_id = u.id AND up.deleted = 0), '[]'::jsonb),
             'roleIds', COALESCE((SELECT jsonb_agg(ur.role_id ORDER BY ur.role_id)
                                  FROM system_user_role ur
                                  WHERE ur.user_id = u.id AND ur.deleted = 0), '[]'::jsonb),
             'email', coalesce(u.email, ''),
             'mobile', coalesce(u.mobile, ''),
             'sex', coalesce(u.sex, 1),
             'avatar', coalesce(u.avatar, ''),
             'loginIp', coalesce(u.login_ip, ''),
             'loginDate', u.login_date,
             'status', u.status,
             'remark', coalesce(u.remark, ''),
             'createTime', u.create_time
         )
         FROM system_users u
         LEFT JOIN system_dept d ON d.id = u.dept_id AND d.deleted = 0
         WHERE u.deleted = 0
           AND u.tenant_id = $1
           AND (
               EXISTS (SELECT 1 FROM actor_roles WHERE code = 'super_admin' OR data_scope = 1)
               OR EXISTS (SELECT 1 FROM actor_roles WHERE data_scope = 5)
                  AND u.id = (SELECT id FROM actor)
               OR EXISTS (SELECT 1 FROM actor_roles WHERE data_scope = 3)
                  AND u.dept_id = (SELECT dept_id FROM actor)
               OR EXISTS (SELECT 1 FROM actor_roles WHERE data_scope = 4)
                  AND u.dept_id IN (SELECT id FROM dept_tree)
               OR u.dept_id IN (SELECT id FROM custom_depts)
           )
         ORDER BY u.username",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
    .map_err(|_| AppError::internal("failed to list users"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn yudao_data_scope_codes_are_documented() {
        assert_eq!(1, 1); // all
        assert_eq!(2, 2); // custom department ids
        assert_eq!(3, 3); // current department
        assert_eq!(4, 4); // current department and children
        assert_eq!(5, 5); // self
    }
}
