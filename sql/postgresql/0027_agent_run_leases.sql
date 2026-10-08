-- 0027: durable 迁移第二阶段。Agent 运行获得租约/心跳与跨副本取消
-- 标志：运行循环每轮续租 90 秒；stop 先置 cancel_requested 再走同进程
-- abort；recover_stale 只把租约过期且非本进程活跃的运行判为 interrupted。
ALTER TABLE toonflow.agent_runs
    ADD COLUMN IF NOT EXISTS lease_until timestamptz,
    ADD COLUMN IF NOT EXISTS heartbeat_at bigint,
    ADD COLUMN IF NOT EXISTS cancel_requested boolean NOT NULL DEFAULT false;
