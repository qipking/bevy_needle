# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added
- **PR-B：Driver 契约与 rig 适配**（规格 v5；feature 分层 `escalate`=能力 / `rig`=实现）：
  - **升级能力层** `src/escalate/`（`escalate` feature，**无 rig 依赖**）：
    - `driver.rs`：`Driver` trait（**无 poll**——submit/cancel + channel 回灌）、
      `DriverId`（RunResolution 只认 id 不认 provider）、`DriverError`（分类：
      Unavailable/Protocol/Transport/Model，不压成单一 Engine）、
      `DriverAttemptCtx`（转录/工具集快照）、`DriverOutcome`/`DriverEvent`
      （事件强制携带 `(run, epoch)`，stale 丢弃）；
    - `state.rs`：`EscalationState` 组件（tier/driver_id/epoch/attempt/deadline/
      started/reason/handle——上下文与 `RunStatus` 分离；tier 是**下标不是 rank**，
      attempt 与 tier 分开计数）；
    - `registry.rs`：`DriverRegistry`（tier → DriverId 映射；模型构造能力在此层，
      policy 保持纯决策）；
    - `bus.rs`：`DriverEventBus`（channel 回灌，ECS 只 drain）；
    - `coordinator.rs`：`driver_coordinator` 系统（submit/drain/单档超时/协作式
      取消/终态规则——「没试/不许试→Escalated」「试过且坏了→Failed」；排在
      `finalize_escalations` 之前）；
    - `mock.rs`：`MockDriver` 脚本化 driver——**无 rig、无网络跑通完整升级链路**；
    - e2e：成功→Completed、末档失败→Failed、无 driver→Escalated（安全阀）。
  - **rig 实现层** `src/rig/`（`rig` feature，git-pinned rig-core/rig-run + tokio）：
    - `agent.rs`：`RigDriverState` 组件——`AgentRun` 是**一次 attempt 的内部
      状态**（Bevy Run ⊃ AgentRun；每次 attempt 重建，history 从转录快照按
      `ChatMessageSeq` 读）；`rig_step_system` 步进（CallModel→等待模型回灌 /
      CallTools→**翻译为 ECS ToolInvocation**/Done→Succeeded）；invalid
      tool-call recovery（大小写 Repair 优先，否则 Retry 回灌纠正反馈）；
    - `tool_bridge.rs`：`PortableDynamicTool` 绊线注册（rig 侧回调永不成功
      ——I3 类型化）；`preresolved_result` 不进 ECS 直接回灌（I19）；
      `tool_call.id` **严格往返** `ToolCall.call_id`，缺失报错禁止 mint（I18）；
      回喂前按原始调用顺序重排（I20）；
    - `transcript.rs`（M1）/ `session_bridge.rs`（M2 `ConversationMemory`）/ 
      `runtime.rs`（懒 tokio，`block_on` 仅 worker 侧——I22）/ `transport.rs`
      （R2→R3 切换面）；
    - `model_turn_with_tools`：`ModelTurn` 规范构造入口（executable/allowed
      集合必须来自 advertise 的工具集，留空会把所有 tool call 判非法）。
  - e2e（rig 工具路径）：注入 `ModelTurn` → `CallTools` → ECS invocation
    （handler 执行、call_id 往返）→ 结果按序回喂 → 下一轮 → Done → Completed。
- `EscalationPolicy::below_threshold`：f64/f32 cast 的**唯一允许入口**
  （规格 §10），app.rs 门控改走它。

### Changed
- **breaking（随 PR-B 一次性完成，规格 §2.3 / P0-6 维护者裁决）**：
  `EscalationTarget` 从 `Needles / Local(LazyModel) / Remote(LazyModel)`
  改为 **`Needle / Local / Remote`（无载函）**——去 execution 泄漏（policy
  回归纯决策，模型构造能力归 `DriverRegistry`）+ 拼写修正（与 `rank()` 文档
  一致）。0.2.0 中 `Local/Remote` 载函位本就不可实现，破坏面为零。
- **breaking（feature 重命名/拆分，规格 §9）**：原脚手架 `escalate`（含 rig
  依赖）拆为 `escalate`（能力层，无 rig）+ `rig`（实现层，
  `rig = ["escalate", "dep:rig-core", "dep:rig-run", "dep:tokio"]`）。
  原 `escalate-local` 未迁移：pinned rev 的 rig-core 已无 per-provider
  features（#2397），本地模型档待 rig 0.43。
- `finalize_escalations` 收窄为**纯安全阀**（不变量 I9）：只把**未被认领**
  （无 `EscalationState`）的 Escalating 收束为 Escalated；已认领的在途
  attempt 由 coordinator 的 deadline/总预算/终态规则保证收敛。无 `escalate`
  feature 时行为与 0.2.0 完全一致。

### Fixed
- rig-run 协议冒烟暴露：`AgentRun` 默认 `max_turns = 1`，工具回喂后的续轮
  会立刻 `MaxTurnsError`——rig attempt 现按 needle `max_steps` 语义取 8。

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
