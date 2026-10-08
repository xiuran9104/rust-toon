# Durable Worker 迁移方案

> 状态：第一、二阶段已落地（2026-10-07，迁移 0026/0027）；第三阶段待排期。
> 目标：消除 Gateway 单副本限制（AGENTS.md 中记录的架构约束），使
> Agent/Workflow 运行时可以水平扩展并在进程崩溃后恢复。

## 现状与问题

Gateway 进程内仍持有两类运行时状态：

1. `ACTIVE_RUNS`（`toonflow_agents.rs`）：Agent 运行的 `AbortHandle` 注册表。
   取消/中止依赖同进程句柄；进程重启后运行标记为 interrupted，但无法续跑。
2. ~~`FLOW_DATA_CACHE`（`toonflow_agent_tool_utils.rs`）~~：**已于第一阶段迁入
   `toonflow.agent_flow_cache` 表（0026）**。多副本网关对“数据未变化”的
   判定现在保持一致。

已具备的持久化基础：`toonflow.agent_runs`（状态/输入/输出/重试关联）、
`toonflow.agent_events`（阶段消息）、`toonflow.distributed_jobs`
（NATS JetStream 租约作业，toon-worker 已消费 `toon.video_quality` 等）、
断点续跑恢复指令（对齐方案 P1，2026-10-07）。

## 第二阶段：Agent 运行注册表持久化（已落地，迁移 0027）

- `agent_runs` 新增 `lease_until`/`heartbeat_at`/`cancel_requested`；
  `run_with_tools` 每轮续租 90 秒并感知跨副本取消（置位即返回“用户已中止”）。
- `stop` 先落取消标志（任一副本的运行循环下一轮感知），再走同进程
  abort 即时路径。
- `recover_stale` 改为租约式：仅租约过期且不在本进程活跃集的运行判
  interrupted；其他副本仍在心跳的运行不受影响。重试走既有断点续跑。

- `agent_runs` 增加租约列（`lease_owner`、`lease_until`、`heartbeat_at`），
  运行中的 Agent 定期心跳续租；租约过期的运行由任一网关实例标记
  interrupted，用户重试即走既有断点续跑路径。
- 取消语义：`cancel` 接口从“同进程 abort”改为“写取消标志 + 广播”，
  运行方在工具调用间隙检查标志；同进程时保留即时 abort 作为快速路径。
- SSE 流：`agent_events` 已持久化，流式输出改为事件表尾随读
  （或 NATS subject），任意副本都能承接断开的 WebSocket。

## 第三阶段：执行体迁移到 toon-worker

- Agent 的模型调用循环改造成 `distributed_jobs` 的一种 kind
  （如 `toon.agent_run`），复用 worker 的租约/重试/心跳协议；
  Gateway 退化为无状态入口（鉴权、SSE 转发、HTTP API）。
- 验收：两副本 Gateway 负载均衡下杀掉其一，进行中的 Agent 运行
  由 worker 继续推进；`AGENTS.md` 的单副本约束解除。

## 风险与顺序

第二阶段不动执行位置，风险最低，先行；第三阶段涉及模型调用方
（网关→worker）与 SSE 通路改造，需要专门会话与完整回归
（`test-toonflow-alignment.sh` + 生产 e2e + 分布式作业 e2e）。
