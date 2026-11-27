-- 0032: SaaS 基础权限变更（自 rustset 0084/0085/0086 移植）。
-- 1) 角色挂载部门树：system_role.dept_id（0 = 全局/未挂载，维持存量行为）。
--    挂载后两条分配规则（Rust 侧 role_binding 校验）：
--      1. 角色只能分配给「挂载节点子树内」的用户；
--      2. 非公司级管理员（super_admin / 任一角色 data_scope=1）只能发放
--         「挂载节点落在自己部门子树内」的角色——发放权不逆向流。
--    数据权限（data_scope 1-5）语义不变，仍相对用户自己的部门解析。
-- 2) 租户管理员部门绑定：存量租户补建以租户名命名的根部门；tenant_admin
--    （最早持有人）dept_id 为空的绑到根部门——管理员是普通用户，出现在
--    本租户用户管理的部门树中。
-- 3) 租户管理员救援权限：system:tenant:update-password（菜单 1273，挂
--    租户管理 1138 下），授予 super_admin；对应 compat 的重置密码端点。
-- 4) tenant_admin 的 role_menu 按当前套餐增量补齐（只加不删；套餐裁剪由
--    authorized_menus 的运行时交集即时生效）。package_id=0 视为未挂套餐，
--    跳过补齐。

ALTER TABLE public.system_role
    ADD COLUMN IF NOT EXISTS dept_id bigint NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_system_role_dept
    ON public.system_role (tenant_id, dept_id) WHERE deleted = 0;

INSERT INTO public.system_menu
    (id, name, permission, type, sort, parent_id, path, icon, component, component_name,
     status, visible, keep_alive, always_show, creator, updater)
SELECT 1273, '租户管理员密码重置', 'system:tenant:update-password', 3, 7, 1138, '', '', '', NULL,
       0, true, true, true, 'system', 'system'
WHERE NOT EXISTS (
    SELECT 1 FROM public.system_menu WHERE id = 1273 AND deleted = 0
);

INSERT INTO public.system_role_menu (id, role_id, menu_id, creator, updater, deleted, tenant_id)
SELECT nextval('public.system_role_menu_seq'), r.id, 1273, 'system', 'system', 0, r.tenant_id
FROM public.system_role r
WHERE r.deleted = 0 AND r.status = 0 AND r.code = 'super_admin'
  AND NOT EXISTS (
      SELECT 1 FROM public.system_role_menu
      WHERE role_id = r.id AND menu_id = 1273 AND deleted = 0
  );

-- 存量租户没有顶级部门的，以租户名补建（tenant_id = 租户）。
INSERT INTO public.system_dept
    (id, name, parent_id, sort, phone, email, status, creator, updater, tenant_id)
SELECT nextval('public.system_dept_seq'), t.name, 0, 0, '', '', 0, 'system', 'system', t.id
FROM public.system_tenant t
WHERE t.deleted = 0
  AND NOT EXISTS (
      SELECT 1 FROM public.system_dept d
      WHERE d.tenant_id = t.id AND d.parent_id = 0 AND d.deleted = 0
  );

-- 租户管理员（tenant_admin 最早持有人）dept_id 为空的，绑到本租户最早的
-- 顶级部门。
WITH admins AS (
    SELECT u.tenant_id, MIN(u.id) AS admin_id
    FROM public.system_users u
    JOIN public.system_user_role ur ON ur.user_id = u.id AND ur.deleted = 0
    JOIN public.system_role r ON r.id = ur.role_id AND r.deleted = 0
         AND r.code = 'tenant_admin' AND r.tenant_id = u.tenant_id
    WHERE u.deleted = 0
    GROUP BY u.tenant_id
), roots AS (
    SELECT tenant_id, MIN(id) AS dept_id
    FROM public.system_dept
    WHERE parent_id = 0 AND deleted = 0
    GROUP BY tenant_id
)
UPDATE public.system_users u
   SET dept_id = roots.dept_id, updater = 'system', update_time = now()
  FROM admins
  JOIN roots ON roots.tenant_id = admins.tenant_id
 WHERE u.id = admins.admin_id
   AND u.tenant_id = admins.tenant_id
   AND u.dept_id IS NULL
   AND u.deleted = 0;

-- tenant_admin 的 role_menu 按当前套餐增量补齐（只加不删）。
INSERT INTO public.system_role_menu
    (id, role_id, menu_id, creator, updater, deleted, tenant_id)
SELECT nextval('public.system_role_menu_seq'), r.id, m.menu_id::bigint,
       'system', 'system', 0, r.tenant_id
FROM public.system_role r
JOIN public.system_tenant t ON t.id = r.tenant_id AND t.deleted = 0
JOIN public.system_tenant_package p
  ON p.id = t.package_id AND p.deleted = 0 AND p.status = 0
CROSS JOIN LATERAL jsonb_array_elements_text(p.menu_ids::jsonb) m(menu_id)
WHERE r.code = 'tenant_admin' AND r.deleted = 0
  AND NOT EXISTS (
      SELECT 1 FROM public.system_role_menu
      WHERE role_id = r.id AND menu_id = m.menu_id::bigint AND deleted = 0
  );
