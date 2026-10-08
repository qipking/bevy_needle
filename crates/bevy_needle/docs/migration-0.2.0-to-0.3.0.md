# MIGRATING — 0.2.0 → 0.3.0（Rig 0.44 执行平面迁移）

> `migration verification` 三件之一（升级计划 §29.7）。读者：从 0.2.x
> 升到 0.3.x 的宿主。读完应当能回答：哪些 API 没了、去哪了、
> `RunStatus` 的两个新变体如何匹配、删除 legacy 后代码怎么写。

## 怎么读

0.3.0 的架构分层：**bevy_needle 保留** ① Needle native（FFI · worker ·
session · codec）② `NeedleSecurityPolicy`（LocalModelOnly / capability
ceiling——薄安全层）③ 薄 model adapter（`Needle3Model` =
`Model<Wire, Transport>`）④ 注册胶水。**rig-ecs 拥有** Agent / Run /
Turn / Effect / Tool dispatch / cancellation / checkpoint（升级计划
§26.5 ownership 二分）。

## 1. `RunStatus::Escalating { tier }` / `RunStatus::Escalated` 怎么办

两个变体**随枚举消失**（legacy run 状态机整体退休），但**语义保留**、
表达法升级：

| 0.2.0（legacy RunStatus） | 0.3.0（rig-ecs 执行平面） | 宿主对策 |
|---|---|---|
| `RunStatus::Escalating { tier }` | rig-ecs 的 run **没有这一中间态**：升级语义由宿主 policy 驱动——切换 `UsesModel`（run 级覆盖，rig-ecs 原生为止；带 epoch / 取消语义，§29.6） | 原来 `match RunStatus` 的 Escalating 分支删除 |
| `RunStatus::Escalated`（正常收尾） | **保留概念，换载具**：local-only 被 policy 拒绝 → rig-ecs 原生 `agent::Cancelled("security: …")` → `Failed(Failure::Cancelled(report))`（report 文本含 `security:` / `LocalModelOnly` 可判别） | 读 `Failed(Failure::Cancelled)` |
| `RunStatus::Completed` | `rig_ecs::agent::Settled` + `RunResult(String)` | 概念不变 |
| `RunStatus::Failed` | `rig_ecs::agent::Failed(Failure)`（九个分类成员） | 概念不变 |
| `RunStatus::Cancelled` | `rig_ecs::agent::Failed(Failure::Cancelled(report))` | **注意合并**：Cancelled 并进 Failed 的一个成员（rig-ecs 里逻辑取消与失败同形） |

### 破坏性 match 修复样例

```rust
// 0.2.0
match run_status {
    RunStatus::Completed => { /* … */ }
    RunStatus::Escalated => { /* 升级终态 */ }
    RunStatus::Failed => { /* … */ }
    RunStatus::Cancelled => { /* … */ }
    _ => { /* … */ }
}

// 0.3.0（rig-ecs）
match (&settled, &failed) {
    (_, Some(Failed(Failure::Cancelled(report)))) => { /* 升级/安全终态（Escalated 语义） */ }
    (Some(Settled), _) => { /* Completed */ }
    (_, Some(failed)) => { /* Failed（Provider/Content/MaxTurns/…） */ }
    (None, None) => { /* 仍在途 */ }
}
```

**迁移提示（编译期报错即迁移到位）**：0.3.0 直接删除 `RunStatus`
枚举及其全部变体。下游 `match RunStatus::…` 的代码将**不再编译**——
这是有意的（§28.4 裁决：默认 failure-closed 迁移）；按上表逐行改写。
`Failure` 的九成员是唯一的新转义出口，没有静默漏分支空间。

## 2. API 替代关系表（删除 legacy 后）

| 0.2.0 API（legacy） | 0.3.0 替代 | 说明 |
|---|---|---|
| `agent::NeedleAgentSpec` / `spawn_agent(world, spec)` | `bevy_needle::rig::spawn_agent(commands, AgentSpec)` + `bevy_needle::rig::register_local_model(world, label, model, gate)` | rig-ecs agent（`Owner / Preamble / UsesModel / Grant…`） |
| `run::RunAgent`（消息发起 run） | `world.spawn_run(agent, history, prompt, streamed, max_turns)`（`rig_ecs::systems::RunCommands`） | rig-ecs 原生 run 发起 |
| `run::RunStatus`（7 变体） | `rig_ecs::agent::{Settled, Failed(Failure), RunPhase…}` | 见 §1 状态映射表 |
| `app::BevyNeedlePlugin`（五段调度表） | `app.add_plugins(rig_ecs::RigPlugin::default())` + `bevy_needle::rig::register_local_model(...)` + `bevy_needle::rig::install_security_guard(&mut app)` | 调度归 rig-ecs `RigSchedule`；bevy_needle 只装模型与注册胶水 |
| `tool::ToolSpec` / `ToolBundle::new(spec)`（schema 注册） | B1：`bevy_needle::rig::register_tool_fn(handlers, name, description, json_schema, callback)`；B2：`register_world_tool(handlers, name, description, params)`（`register_open(Tool family)` + `WorldOutcome`） | **schema 唯一来源 = rig `ToolDefinition`**（§26.6 已裁） |
| `tool::register_tool_handler(world, name, fn)`（legacy `ToolHandlerFn`） | B1 轨的 `callback: impl Fn(&mut ToolContext, Value) -> BoxedFuture<…>`——**直接为 rig `ToolFn`**，不再有第二个函数表 | 桥接违规（`PLACEHOLDER` + 空 call_id）已删（§27.3） |
| `tool::ToolCall { call_id, … }`（自铸 ID） | rig `CallId`（`needle-call-<seq>`，`codec::call_id` 单点 mint；I18 v27 两侧同值） | 内容图 `ContentPart::ToolResult { call, … }` 原样往返 |
| `tool::ToolCallCompleted / ToolCallFailed`（消息面） | `rig_ecs::bus::EffectOutcome` / 内容图 `ToolResult` part（含 `ToolResultStatus`） | 游戏侧用观察者 / 普通系统读取 |
| `run::RunNote` | `Failed(Failure)` report 文本 / `agent::Cancelled(reason)` | 文本可判别（`security:` 前缀） |
| `needle_runtime::NeedleRuntime`（legacy worker 桥） | `bevy_needle::rig::Needle3Worker`（I22 worker；I27 回执；§29.3 shutdown/join） | 会话语义不变（一个 Agent = 一个会话）；事件面改为 per-提交等待 future |
| `policy::EscalationPolicy`（legacy 档位表 + confidence 阈值） | `bevy_needle::rig::NeedleSecurityPolicy`（**LocalModelOnly 保留**——§26.3 实测：rig capability 无法承载 local-only，只能留本 crate）+ `register_local_model(gate: Option<ConfidenceGate>)`（confidence 阈值移到 gate 注册参数） | tier 概念取消（升级语义由宿主 policy 驱动 rig 切换 `UsesModel`） |
| escalate 层（Driver trait / DriverRegistry / Coordinator / MockDriver） | **删除**（唯一消费者是 legacy run 状态机）；升级语义由 rig 执行宿主 policy 表达 | 隐私红线（LocalModelOnly 留本 crate）语义不变 |
| `BevyNeedlePlugin::with_backend`（mock 后端） | `bevy_needle::rig::needle3_model(Arc::new(backend), label)` 的 backend 参数（`NeedleBackend` trait 保留） | mock 注入点不变 |
| `run::RunEscalation / RunEscalated` / `run::CancelRun`（消息） | 无直接对应——升级触发的语义留在宿主 policy 面；取消语义归 `rig_ecs` 的 `cancel_run` | `Failure::Cancelled(report)` 文本可判别拒绝缘由 |

**整体消失（无替代，仅列出防漏迁）**：`AgentToolRefs` / `PrimarySession`
/ `RunSession` / `RunOwner` / `RunPendingInput` / `RunEngineInFlight` /
`schema.rs`（参数校验——rig 直接吃 JSON Schema）/ `engine_index.rs`
（agent 快照索引——rig 广告面取代）。`Needle3Model::run()` 形态从未公开。

## 3. 迁移后行为对照（行为角度验证清单）

- **网络红线（LocalModelOnly）**：fail-closed 强化（§29.2）——未分类
  handler 也拒绝，不是"没登记就不看"；注册面只有
  `register_local_model` / `register_remote_model`（事务式）。
- **置信度门控**：effect 级验收（§27.4）——`low → ToolEffect == 0 且
  WorldEffect == 0`；`high → ToolEffect == 1`；respond 轮永不门控。
- **取消**：`cancel_run` → `Failed(Cancelled)` 终态 + epoch 防线（§29.6）。
- **worker**：`shutdown()/join` 幂等生命周期（§29.3）；Drop 只停收。
- **call_id**：规范形态 `needle-call-<seq>`，rig `CallId` 与 ECS 两侧
  同值往返（I18 v27）。
