# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

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
