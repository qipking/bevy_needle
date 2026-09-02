# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

## [0.2.0] - 2026-09-08

### Changed
- **置信度门控语义**（升级契约落地第一步）：低于门限的调用不执行，run 走
  `Escalating → Escalated` 正常收尾并发 `RunEscalation` 消息，转录落
  "[已升级]"；**`Failed` 回归纯引擎/调度错误语义**（原"门控也进 Failed"废止）。
  `RunStatus` 新增 `Escalating { tier }`（升级在途）与 `Escalated`（升级终结，
  终态、不可取消改写）两个变体；新增 `RunEscalated` 消息、
  `persist_escalated_runs` 收尾系统与 `finalize_escalations`
  （RunResolution 内，无更多档位时直接终结、绝不静默悬挂）。
- **MSRV 1.85 → 1.98**：原声明与实际要求不符（bevy 0.19 要求 ≥1.95；
  1.98 为 rig-core 0.42 的 MSRV，为升级路径预留）。

### Added
- `policy.rs`（无 cfg、无 rig 依赖）：`EscalationPolicy` / `OnlineFallback`
  / `EscalationTarget`（`Needle → Local → Remote`，`Ord` 只按能力档位次，
  档位单调递增永不回退；`Local/Remote` 载函为 escalate feature 落地前的
  占位形态）。插件默认 `init_resource` 注入离线默认值。
- `RuntimeDiagnostics::runs_escalated` 计数与 `summary()` 中的 `⇧` 列。
- README：crates.io / docs.rs / License / MSRV 徽章。

### Fixed
- workspace 成员路径指向已不存在的 `examples/univis_needle_demo`
  （演示工程实际位于 `demo/`），workspace 解析恢复。
- README 与实际布局漂移：架构文档链接改指 `crates/bevy_needle/docs/`，
  移除不存在的 `--features link` 与 `scripts/fetch_engine.sh` 引述。

## [0.1.0] - 2026-09-01

### Added
- `BevyNeedlePlugin`：`EngineSync → RunPreparation → RunExecution → RunCommit → Telemetry`
  调度管线；引擎解码在独立工作线程串行执行，ECS 每帧只提交/收割。
- ECS 建模（对齐 bevy_rig）：`NeedleAgent` / `ToolSpec` / `Session` / `Run` /
  `ToolInvocation` 实体组件；`RunAgent` / `ToolCallCompleted` 等消息。
- `NeedleBackend` trait + `DlopenBackend`（默认 `dlopen` feature）+ `MockBackend`
  （脚本化信封 / dynamic 闭包，完整轮次循环可无引擎测试）。
- 工具 schema 构建 `ParametersBuilder`（枚举/区间编译进解码语法）与规范化校验。
- 多轮工具回喂、`max_steps` 上限、置信度门控（`RunEscalation`）、`ResetAgent`、
  调优 `.cact` 权重一次性加载、工具检索索引缓存路径。
- 引擎缺失优雅降级：状态 `Unavailable`，run 以可操作原因失败，绝不 panic。
- 21 个测试：schema/registry/session 单元 + MockBackend 全循环 10 项
  （单轮/多轮/门控/max_steps/未知工具回喂/引擎缺失/重绑/重置）。
- `examples/`：headless_echo、tool_dispatch（多轮+门控）、extraction；
  `examples/univis_needle_demo`：文字操控 univis_ui 的完整演示（含 autopilot 自检）。

### Fixed
- 显式引擎路径缺失时立即报错（权威路径语义），不再静默回退。
- `TurnJob` 使用 `Arc<str>`，避免每轮克隆工具集字符串。
- EngineSync 只在 agent/工具集变化时重建快照（变更驱动，非每帧全量）。
