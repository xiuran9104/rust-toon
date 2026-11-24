-- 0031: durable 第三批。workflow 运行与节点获得租约/心跳与跨副本取消标志：
-- 节点任务在检查点续租 90 秒并感知 cancel_requested；cancel 先置标志再走
-- 同进程 abort；周期回收只把租约过期且非本进程活跃的节点/运行判为失败，
-- 进程崩溃遗留的 running 行不再依赖用户请求才被清理。
ALTER TABLE toonflow.workflow_runs
    ADD COLUMN IF NOT EXISTS lease_until timestamptz,
    ADD COLUMN IF NOT EXISTS heartbeat_at bigint,
    ADD COLUMN IF NOT EXISTS cancel_requested boolean NOT NULL DEFAULT false;

ALTER TABLE toonflow.workflow_node_runs
    ADD COLUMN IF NOT EXISTS lease_until timestamptz,
    ADD COLUMN IF NOT EXISTS heartbeat_at bigint,
    ADD COLUMN IF NOT EXISTS cancel_requested boolean NOT NULL DEFAULT false;

CREATE INDEX IF NOT EXISTS idx_workflow_runs_running
    ON toonflow.workflow_runs (project_id, script_id, start_time DESC)
    WHERE state = 'running';

CREATE INDEX IF NOT EXISTS idx_workflow_node_runs_running
    ON toonflow.workflow_node_runs (workflow_run_id, start_time DESC)
    WHERE state = 'running';
