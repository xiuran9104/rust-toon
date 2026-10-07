# 分镜制作同步修复计划

更新日期：2026-08-11

对照目标：`/home/xiuran/code/Toonflow-app` 1.1.8 的分镜制作流程。

审计记录：`/.codex/audit/storyboard-sync/report.md`。

## 当前状态

### 已完成

- 完成 Rust Toon 与 Toonflow-app 分镜制作页面、接口和数据结构的首轮对比。
- 新增独立 `StoryboardPanel.vue`，避免继续把分镜交互堆入项目详情页。
- 分镜卡片已接入：
  - 单条选择与全选。
  - 单条生成和失败重试。
  - 批量生成。
  - 强制重新生成。
  - 单条删除和批量删除。
  - 编辑分镜描述和图片提示词。
  - 生成状态、失败原因展示。
- 已接入分镜生成状态轮询，生成完成后自动更新卡片，不再依赖手动刷新。
- 后端批量新增分镜已按 `track` 分组：
  - 同组分镜共享 `trackId`。
  - 自动创建视频轨道。
  - 自动汇总组内分镜时长。
- 单条和批量删除分镜时已增加：
  - 清理分镜与资产关联。
  - 清理图片编辑流。
  - 删除空视频轨道。
  - 非空轨道重新汇总时长。
- `cargo fmt --check` 通过。
- `cargo test --workspace` 通过；仅存在未使用代码等 warning。
- 前端全量 TypeScript 检查通过。
- 空库数据库迁移测试通过。
- 已增加数据库集成测试，覆盖分镜 Track 分组、时长重算、移动分组、关联清理和空轨道删除。
- 已增加生产链路 E2E，覆盖项目、小说、剧本、资产、分镜、轨道、候选视频和 FFmpeg 成片导出。
- 图片编辑流可从资产和分镜进入，保存后支持回写图片。
- 分镜预览及选中/全部图片下载已经接入。

### 当前验证限制

- 自动化生产链路使用本地 FFmpeg 视频夹具，不调用真实 AI 图片/视频供应商；供应商鉴权、计费、限流和长轮询仍需单独验证。
- 浏览器端尚缺覆盖完整工作台交互的自动化测试，响应式与键盘操作主要依赖人工回归。
- Toonflow-app 1.1.8 的截图对照属于 2026-07 的审计基线；参考项目升级后需要重新审计差异。

## 后续计划

### P0 收尾：功能闭环与回归验证

1. 启动 Toonflow-app 当前参考版本并重新确认基线。
2. 使用浏览器实际走通 Toonflow-app 分镜流程并重新截图：
   - 进入项目与选择剧本。
   - 分镜表生成完成状态。
   - 分镜卡片选择与批量操作。
   - 单条生成、失败状态和重试。
   - 合成预览与图片编辑流入口。
3. 在 Rust Toon 测试项目中使用真实供应商验证：
   - 轮询是否从“生成中”正确收敛到“已完成”或“生成失败”。
   - 图片和视频供应商错误是否正确进入失败重试。
4. 增加浏览器 E2E，覆盖选择、批删、生成、失败重试和轮询。
5. 持续运行：

```bash
cargo test --workspace
bash script/test-database-migrations.sh
bash script/test-production-e2e.sh
pnpm --dir apps/web --filter @vben/web-antd run typecheck
pnpm --dir apps/web --filter @vben/web-antd run build
```

### P1：同步 ToonFlow 工作台结构

1. 调整分镜制作整体布局：
   - Agent 面板支持折叠或拖动宽度。
   - 主画布设置最小可用宽度。
   - 1440px 视口下不再互相覆盖。
2. 改造固定五阶段画布：
   - 减少固定绝对坐标依赖。
   - 增加适应视图、定位当前阶段和重置缩放。
   - 分镜表与分镜卡片可以快速互相定位。
3. 将分镜按 Track 分组显示：
   - 展示组时长与分镜数量。
   - 支持组内选择和整组生成。
   - 展示轨道与视频制作阶段的对应关系。
4. 完善编辑能力，使时长、Track、关联资产、是否生成图片均可更新；修改后同步重算轨道。

### P2：视觉、响应式与无障碍

1. 修复 Agent 消息区白底浅色文字和深色主题对比度。
2. 为所有图标按钮补充可访问名称和 Tooltip。
3. 为画布节点、卡片选择、生成状态变化提供键盘操作和明确焦点样式。
4. 为生成成功、生成失败和批量删除结果增加屏幕阅读器可感知的状态通知。
5. 验证 1280px、1440px、1920px 和窄屏布局。
6. 避免通过过低默认缩放容纳全部节点，保证分镜正文默认可读。

## 建议实施顺序

1. 完成 Toonflow-app 启动和浏览器实测。
2. 完成真实供应商和浏览器端 P0 回归测试。
3. 实施 P1 布局和 Track 分组。
4. 完成 P2 无障碍和响应式收尾。

## 涉及文件

- `apps/web/apps/web-antd/src/views/toonflow/projects/StoryboardPanel.vue`
- `apps/web/apps/web-antd/src/views/toonflow/projects/ProductionFlowCanvas.vue`
- `apps/web/apps/web-antd/src/views/toonflow/projects/detail.vue`
- `apps/web/apps/web-antd/src/api/toonflow/index.ts`
- `crates/modules/toon-server/src/toonflow.rs`
- `crates/modules/toon-server/src/toonflow_image_workflow.rs`
