-- 0026: durable 迁移第一阶段。Agent 工具的流数据变化检测缓存从
-- Gateway 进程内存迁入数据库，使多副本网关对“数据未变化”判定一致。
CREATE TABLE IF NOT EXISTS toonflow.agent_flow_cache (
    isolation_key text NOT NULL,
    key text NOT NULL,
    value text NOT NULL,
    update_time bigint NOT NULL,
    PRIMARY KEY (isolation_key, key)
);
