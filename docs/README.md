# 文档导航

Rust Toon 文档按职责拆分。完整步骤和详细规则只在一个权威文档中维护；入口文档只保留必要摘要并链接到权威来源，避免启动命令、默认值和部署约束相互漂移。

## 权威来源

| 内容 | 权威文档 |
| --- | --- |
| 项目简介、五分钟启动、常用验证 | [根 README](../README.md) |
| AI 编码代理的仓库约束与变更流程 | [AGENTS.md](../AGENTS.md) |
| 本地、systemd、Compose、Kubernetes、备份和排障步骤 | [deployment.md](deployment.md) |
| 环境变量、默认值、动态配置和本地基础设施 | [configuration.md](configuration.md) |
| 当前架构、模块边界和运行模型 | [technical-solution.md](technical-solution.md) |
| 已实现范围与验收边界 | [parity-roadmap.md](parity-roadmap.md) |

## 专题与计划

- [媒体生成稳定性计划](media-generation-stability-plan.md)
- [Toonflow Agent 与内容链路对齐方案](toonflow-agent-alignment-plan.md)
- [剧本 Agent 原著核实与自动修订](script-agent-quality-improvements.md)（已落地行为说明）
- [通用真人影视基底参考图](live-action-base-reference.md)

计划文档记录阶段性目标，不作为当前运行行为或部署参数的权威来源。若计划描述与代码、迁移或上述权威文档冲突，以当前代码和迁移为准，并同步修正文档。

## 历史归档

一次性审查快照与已完成的阶段性计划移入 [archive/](archive/)，仅作过程记录，不代表当前代码状态。

## 维护规则

1. README 只保留最短可运行路径，不复制完整生产配置。
2. 部署命令只写入 `deployment.md`；变量语义和默认值只写入 `configuration.md`。
3. `AGENTS.md` 只保留代理必须遵守的安全约束、数据库流程和验证入口。
4. 已发布的数据库迁移不可修改；`sql/bootstrap/current.sql` 只是审阅快照，不是启动输入。
5. 文档不得声明代码未读取的环境变量。新增或移除配置时，应同时更新环境样例和全仓检索结果。

## 当前本地账号

应用基线迁移创建 `admin`，初始密码为 `admin123`。网关启动只校验启用的超级管理员是否存在，不创建账号，也不会根据环境变量重置密码。账号细节及生产安全要求见[配置说明](configuration.md#112-启动账号说明)。
