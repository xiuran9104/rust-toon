-- 0030: durable 第三阶段。WS 发起的运行同样入库后，重连时的
-- latest_running_run 与周期 recover_stale 都按 running 状态扫描；
-- 部分索引保持极小体积并消除全表顺序扫描。
CREATE INDEX IF NOT EXISTS idx_agent_runs_running
    ON toonflow.agent_runs (agent_type, isolation_key, start_time DESC)
    WHERE state = 'running';
