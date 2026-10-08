# bevy_needle PR-B 重构规格：控制平面 / 执行平面与 Rig 0.44 适配

> 版本：**v23**（2026-10-08）  
> 状态：**Phase 0–2 已实施 / G1–G7 验收通过**（2026-10-08）  
> 面向：Claude Code / 人工复核  
> 基线：bevy_needle 0.2.x 已发布契约 + Rig 0.44.x 当前 API  
> 前置：v22 已接受“控制平面 / 执行平面”框架，但本版本补齐了 0.44 API 时代的硬边界与失败回滚条件。

---

## 0. 裁决摘要

### 0.1 本次目标

PR-B 的目标不是立即重写 bevy_needle，也不是立即删除现有 `Run` / `EscalationPolicy` / `ToolInvocation`。

第一阶段必须完成：

```text
bevy_needle policy
    ↓
Driver / Rig adapter
    ↓
Rig 0.44
    ↓
rig-ecs
    ↓
ECS ToolInvocation
    ↓
Bevy World / AuK
```

只有 POC 全部通过后，才允许退休 `RigDriver` 内部自有 AgentRun 步进代码。

### 0.2 最终职责边界

```text
bevy_needle = 控制平面
────────────────────────────────────
Needle3 native runtime
Needle session / worker
confidence
EscalationPolicy
Capability ceiling
LocalModelOnly / OnlineFallback
Driver identity
attempt / epoch / cancellation
policy gate / privacy boundary
Rig adapter

rig-core / rig-ecs = 执行平面
────────────────────────────────────
model / wire / decoder contracts
Agent / Run / Turn
Effect / Handler
Tool batching / concurrency
World / Task
stream / checkpoint / replay
```

### 0.3 非目标

本 PR 不做：

- 重写 Needle3 native runtime
- 让 Needle 自己执行 Bevy Tool
- 将 `OnlineFallback` 移交给 rig-ecs
- 用 `ToolAccess` 代替隐私/网络能力边界
- 默认引入 rig / tokio / reqwest
- 立刻删除已经发布的 `RunStatus::Escalating / Escalated`
- 构造第二条 Needle global session 通道

---

# 1. 不可协商不变量

以下不变量在 POC 通过前后都成立。

| # | 不变量 |
|---|---|
| I1 | **主线程永不 `block_on`**。ECS system 不得同步等待模型/网络 future。 |
| I2 | **Driver 不是 Tool，不是 NeedleBackend，不是 Agent**。Driver 只表达“一次 attempt 如何取得模型结果”。 |
| I3 | **工具执行必须在 ECS 侧**。Rig 的 `CallTools` 最终必须落入现有 `ToolInvocation` pipeline。 |
| I4 | **Bevy Run ⊃ Agent/rig execution state**。一次 Bevy Run 可以经历多个 attempt、升级、取消、人审。 |
| I5 | **Policy 是纯决策，Driver 是纯执行**。Driver 不决定是否升级；Policy 不持 model/client/runtime。 |
| I6 | **RunResolution 不 match provider**。核心只认识 `DriverId`、状态与结果，不认识 Rig/OpenAI/Candle。 |
| I7 | **Run 不持具体模型对象**。模型生命周期由具体 Driver / adapter 持有。 |
| I8 | **RunStatus 只表达生命周期**；上下文留在 `EscalationState` / attempt snapshot。 |
| I9 | `finalize_escalations` 只做安全收束，不决定模型、不决定重试、不决定 provider。 |
| I10 | **tier 是 `EscalationPolicy.tiers` 的数组下标，不是 capability rank**。 |
| I11 | **默认构建零成本**：default feature 不依赖 rig / tokio / reqwest。 |
| I12 | **MSRV 保持 1.98**，除非另有独立 RFC。 |
| I13 | **禁止依赖已删除/未发布的 `rig-run` 作为当前实现前提**。当前 Rig 版本和源码才是规范。 |
| I14 | Rig history 必须按显式顺序恢复，禁止 Entity id / HashMap iteration order。 |
| I15 | 本 PR 必须复用 0.2.x 已发布的 escalation / ToolInvocation 契约；Breaking 仅限已经明确裁决的形状修正。 |
| I16 | Driver trait 不提供 future polling API；异步结果经 worker/channel 回灌，ECS 只 drain。 |
| I17 | 每个异步 job 必须携带 `(run, epoch)`；epoch 不匹配的事件不得修改 Run。 |
| I18 | tool `call_id` 必须严格原样往返；禁止缺失时 mint 新 ID。 |
| I19 | `preresolved_result` 已存在时禁止进入 ECS tool execution。 |
| I20 | tool results 必须按原始 call order 回喂 Rig。 |
| I21 | Needle 与 Rig 是两个并发域；不能用“复用 Needle 串行队列”为理由把 Rig 全部串行化。 |
| I22 | `block_on()` 只能存在于 worker-private 边界。不得暴露为 ECS 可调用的普通 public API。 |
| I23（修订） | **Local 与 Escalation 必须是分离的 policy path；execution substrate 可以共享。** LocalModelOnly 必须由 bevy_needle 的模型选择/能力策略强制，而不是把隐私保证交给 ToolAccess。 |
| I24 | `CompletionModel` 等旧版模型 trait 不得出现在新 Driver 契约中。模型 adapter 只能存在于 Rig feature 的实现层。 |
| I25 | Driver 必须声明 capability；注册/绑定时校验 capability 与 tier target 兼容。capability 不用于推导 tier。 |
| I26 | Rig model state 只能从唯一 inbox / handoff 入口注入，禁止旁路修改。 |
| I27 | worker 对每一个 request 必须给出成功/失败/传输错误回执，禁止静默丢弃。 |
| I28 | Driver resolution 对一次 attempt 必须稳定；registry mutation 不得改变已经运行的 attempt。 |
| I29 | DriverId 不得覆盖；注册失败必须事务性，无半注册状态。 |
| I30 | 禁止代码通过固定字符串判断某个 Driver 身份。身份必须来自实例字段。 |
| I31 | Rig side 的 driver identity tracking 必须按具体模型类型隔离。 |
| I32 | 一个具体 Rig model 类型可共享一个模型入口/handler，但不同 DriverId 的执行身份必须可区分。 |
| I33 | **Rig 0.44 API 以 pinned local source / cargo metadata 为最高优先级；历史 Appendix 不得作为 implementation API。** |
| I34 | `ToolAccess` 是 tool permission，不得单独宣称为“网络绝不离机”的安全证明。 |
| I35 | POC 失败必须允许原有 bevy_needle escalation 路径继续运行；禁止“先删旧实现再验证”。 |

---

# 2. 当前架构与目标架构

## 2.1 现有主路径

```text
RunAgent
  ↓
RunPreparation
  ↓
RunExecution
  ↓
Needle worker
  ↓
RunResolution
  ├── Completed
  ├── Failed
  ├── Cancelled
  └── Escalating { tier }
```

工具继续经过已有 ECS pipeline。

```text
ToolCall
  ↓
ToolInvocation entity
  ↓
ToolDispatchSystems
  ↓
RegistryHandler / External
  ↓
Tool result
  ↓
RunResolution
```

## 2.2 PR-B 中间架构

```text
Bevy Run
   │
   ├── Needle attempt
   │       └── existing NeedleBackend worker
   │
   └── Escalation
           ↓
      DriverCoordinator
           ↓
      DriverRegistry
           ↓
       RigDriver<M>
           ↓
       Rig adapter
           ↓
        rig-ecs / rig-core
           ↓
       model completion
           ↓
       CallTools
           ↓
     ToolInvocation ECS
           ↓
       tool results
           ↓
       Rig model turn
           ↓
       DriverEvent
           ↓
       RunResolution
```

## 2.3 目标架构

目标不是“统一 Model”。目标是：

```text
        ┌──────────────────────────┐
        │     bevy_needle          │
        │                          │
        │ policy / privacy         │
        │ escalation / tiers       │
        │ Needle worker            │
        │ Driver identity          │
        └────────────┬─────────────┘
                     │
             selected execution path
                     │
                     ▼
        ┌──────────────────────────┐
        │       rig-ecs             │
        │                          │
        │ Agent/Run/Effect/Handler │
        │ Task/World/Checkpoint    │
        └────────────┬─────────────┘
                     │
                     ▼
              Bevy ECS / AuK
```

是否最终退休 `RigDriver` 的内部状态机，由 POC 决定，而非预先决定。

---

# 3. Rig 0.44 事实基线

## 3.1 当前 API

截至 2026-10-08，Rig 0.44.0 的公开 `rig-core` API 仍包含：

```text
rig_core::serve
    Serve
    Layer
    Intercept
    Decision
    Verdict
    adapters::ModelAdapter

rig_core::driver
    Model
    DynModel

rig_core::wire
    Wire
    Decoder
    Operation
```

`ModelAdapter` 仍是把 model 作为 handler/serve 对象接入 Rig 的正式边界；当前 `rig-core` 同时将 `Model` 绑定到 wire/transport，并允许用 `DynModel<Operation>` 进行操作级擦除。具体泛型与构造签名必须以锁定版本的本地源码为准。当前文档明确显示 `rig_core::serve` 与 `ModelAdapter` 存在。 

## 3.2 重要迁移规则

禁止按旧版：

```rust
trait CompletionModel
Box<dyn CompletionModel>
```

继续实现新适配器。

**新代码不得引用旧版 CompletionModel API。**

推荐方向：

```text
Needle3 native
   ↓
rig-core 0.44 wire/operation-compatible adapter
   ↓
Model / DynModel<Completion>
   ↓
ModelAdapter
   ↓
Serve
```

如果 0.44 的精确 `Wire` / `Decoder` / `Model::new` 构造方式与上述图不同，以 pinned source 为准。

## 3.3 rig-ecs 0.44

`rig-ecs 0.44.0` 的定位是：

> rig inside a Bevy World: effects as entities, handlers as entities, the driver as a system.

当前 feature metadata 显示 0.44.0 只有 `assets` 一个可选 feature，默认没有 feature。应用是否启用它由宿主 feature 决定。

## 3.4 当前上游稳定性

Rig 官方仓库明确警告当前仍处于快速演进、未来会有 breaking changes 的阶段。因此本项目必须：

1. pin 明确版本；
2. 使用 cargo lock；
3. 所有 implementation API 都以本地源码/编译结果为准；
4. 不以旧版本 Appendix 推断新 API。

---

# 4. Feature 设计

## 4.1 推荐 Cargo features

```toml
[features]
default = ["dlopen"]

dlopen = ["dep:libloading"]
escalate = []
rig = [
    "escalate",
    "dep:rig-core",
    "dep:tokio",
]
rig-ecs = [
    "rig",
    "dep:rig-ecs",
]
```

精确依赖项名称与版本以当前 workspace resolve 为准。

### 必须通过

```bash
cargo check
cargo check --no-default-features
cargo check --features escalate
cargo check --features rig
cargo check --features rig-ecs
```

并增加：

```bash
cargo tree --no-default-features
```

断言默认/无 feature 构建不存在：

```text
rig-core
rig-ecs
rig-agent
rig-run
reqwest
```

以及不应意外引入新的网络 runtime。

## 4.2 不得把 feature 名当产品语义

`rig` = Rig 实现能力。  
`escalate` = escalation 抽象能力。  
`rig-ecs` = Rig ECS host 实现。

未来添加 `remote` / `human` 等 driver 不得强迫默认用户启用 Rig。

---

# 5. Driver 契约

## 5.1 核心接口

```rust
pub trait Driver: Send + Sync + 'static {
    fn id(&self) -> DriverId;
    fn capability(&self) -> DriverCapability;

    /// Submit an attempt; must return immediately.
    /// Completion is delivered through a host-owned channel.
    fn submit(&self, ctx: DriverAttemptCtx) -> Result<AttemptHandle, DriverError>;

    /// Cooperative cancellation. Underlying model execution may continue.
    fn cancel(&self, handle: AttemptHandle);
}
```

注意：这里是**契约草案**，具体错误类型和字段以现有 v22 master 实际代码为准。不能让 Claude Code 自行重命名既有已发布类型。

## 5.2 Driver 不持有 Policy

禁止：

```rust
struct RigDriver {
    policy: EscalationPolicy,
}
```

禁止：

```rust
if confidence < threshold {
    self.escalate(...)
}
```

正确：

```text
Driver → 产生结果
RunResolution → Policy 判断
Policy → 决定下一 tier
Coordinator → resolve Driver
```

## 5.3 Driver identity

```rust
pub struct RigDriver<M> {
    id: DriverId,
    capability: DriverCapability,
    model: Arc<M>,
    jobs: Sender<ModelJob<M>>,
}
```

不得：

```rust
fn id(&self) -> DriverId { DriverId("rig") }
fn capability(&self) -> DriverCapability { Local }
```

身份必须由实例携带。

---

# 6. DriverRegistry

## 6.1 两层映射

```rust
pub struct DriverRegistry {
    drivers: HashMap<DriverId, Arc<dyn Driver>>,
    tier_bindings: Vec<DriverId>,
}
```

第一层：

```text
DriverId → Driver instance
```

第二层：

```text
tier index → DriverId
```

不要直接把 `tier` 推导成 rank。

## 6.2 register / bind_tier 必须分开

```rust
registry.register(driver)?;
registry.bind_tier(0, id)?;
registry.bind_tier(1, id)?;
```

一个 Driver 可以绑定多个 tier。

`register()` 重复 ID：报错。  
`bind_tier()` 重复 tier：报错。  
显式覆盖：走 `rebind_tier()`，不能隐式覆盖。

两个操作必须事务性：失败后 registry 不留下半注册状态。

## 6.3 Driver attempt-stable

一次 attempt 一旦 resolve：

```text
EscalationState.driver_id = X
```

后续 registry mutation 不得把它改成 Y。

旧 event 是否能落入 Run 继续由：

```text
(run, epoch)
```

决定。

---

# 7. Rig 适配器：0.44 正确路线

## 7.1 禁止旧版 CompletionModel

以下全部视为历史 API，不得写入新实现：

```rust
impl CompletionModel for Needle3Model { ... }
Box<dyn CompletionModel>
```

## 7.2 新适配链

目标是：

```text
Needle3Model / NeedleWire
      ↓
rig-core 0.44 compatible Model
      ↓
DynModel<Completion>（若当前 API 要求）
      ↓
ModelAdapter
      ↓
ErasedHandler / Serve
```

具体实现有两种允许路线：

### 路线 A：Needle 原生请求就是纯 completion

做一个薄 `Wire + Decoder`：

```text
CompletionRequest
     ↓
Needle encode
     ↓
Needle worker
     ↓
Needle JSON response
     ↓
Completion response/event
```

### 路线 B：直接封装现有 worker future

如果 0.44 的当前 model constructor 允许直接绑定一个 host transport，则：

```text
NeedleModel facade
    ↓
worker channel
    ↓
DynModel<Completion>
```

无论 A/B，**都必须复用现有 Needle dedicated worker**；不能让 Rig scheduler 直接调用 FFI 的同步 `needle_complete()`。

## 7.3 Worker 边界

```text
Rig async task
    ↓
Needle3Model
    ↓ channel
Needle worker thread
    ↓
needle_complete()
    ↓
result
    ↓ channel
Needle3Model future resolves
```

I1/I22 的关键是：

```text
async facade ≠ native inference executor
```

### 禁止

```rust
async fn completion(...) {
    blocking_needle_complete(...);
}
```

如果要通过 `spawn_blocking`，必须保证该 blocking 边界仍然位于 worker 层，而不是 ECS system。

---

# 8. Tool schema bridge

## 8.1 Source of truth

Rig 侧的 tool definition 与 Needle3 的 JSON Schema 边界应保持单向转换：

```text
Rig / app tool registry
      ↓
Portable tool descriptor
      ↓
Needle tool JSON schema
```

不要建立第二套“Rig tools registry”。

## 8.2 runtime-defined tools

运行时定义工具必须保留。

目标形态：

```text
name: String
 description: String
parameters: serde_json::Value
```

Rig 当前工具层存在 runtime-defined tool handler/descriptor 能力；若使用 0.44 的具体类型，先以本地源码确定精确构造签名。

## 8.3 工具执行边界

硬规则：

```text
Rig CallTools
   ↓
bevy_needle bridge
   ↓
ToolInvocation
   ↓
现有 ToolDispatchSystems
   ↓
ECS
```

禁止：

```text
Rig async callback
   ↓
直接调用 ToolHandlerFn
```

禁止：

```text
Rig async callback
   ↓
直接 &mut World
```

这样才能保持 I3。

---

# 9. Tool call / result 精确映射

## 9.1 call id

Needle3 如果产生：

```json
{
  "function_calls": [
    {
      "name": "auk.change_pitch",
      "arguments": {"semitones": -2},
      "call_id": "..."
    }
  ]
}
```

Rig 侧必须保持原始 call id；bevy_needle 的 `ToolInvocation.call_id` 也必须保持同值。

如果某一侧必需 ID 而 Needle3 真的没有 provider call id，必须先在 bridge contract 中定义“哪一侧生成、生成一次、何时持久化”，不能随意在工具执行前临时 mint。

本项目现行 I18/I24 约束优先：**缺失 call id 默认拒绝，不自动替换。**

## 9.2 pre-resolved

```text
PendingToolCall.preresolved_result != None
       ↓
不要进入 ECS
       ↓
直接生成 tool result
```

必须有测试证明 ECS execution count = 0。

## 9.3 工具结果顺序

即使 ECS completion order 是：

```text
C → A → B
```

回喂 Rig 必须是：

```text
A → B → C
```

排序 key：**原始 call order / stable call index**。

禁止依赖：

```text
Entity id
HashMap order
completion timestamp
```

---

# 10. Rig 与 Needle 的 session / history

## 10.1 不把 Bevy Run 等同于 Rig/Needle session

```text
Bevy Run #42
├── Needle session / attempt
├── Rig attempt #1
├── Rig attempt #2
└── Human approval
```

## 10.2 Needle history

首次 handoff：

```text
collect_transcript
    ↓
按 ChatMessageSeq
    ↓
snapshot
```

不得使用：

```text
Entity id
HashMap
spawn order
```

## 10.3 Rig history

Rig 内部可以使用它自己的 conversation/effect history，但它必须是：

```text
Rig-local state
```

而不是替代 Bevy Run 的 application-level history。

## 10.4 跨 tier

第一版推荐：

```text
Needle attempt
    ↓
canonical snapshot
    ↓
Rig attempt #1
    ↓
Rig attempt #2
```

跨 tier 的 model state 不应依赖某个 provider 永远保持活跃。

---

# 11. Escalation 与 Policy

## 11.1 tier 语义

```text
policy.tiers = [
    Local(Candle-A),
    Local(Candle-B),
    Remote(OpenAI),
]
```

tier：

```text
0 / 1 / 2
```

rank：

```text
Local = 1
Remote = 2
```

两者绝对不能混为一谈。

## 11.2 capability gate

注册时校验：

```text
EscalationTarget::Local
    ↔
Driver.capability() == Local
```

如果 policy 禁止 Remote：

```text
Remote driver 不可被选中
```

但不要用：

```text
ToolAccess
```

单独替代这一层。

## 11.3 LocalModelOnly

最低安全要求：

```text
OnlineFallback::LocalModelOnly
        ↓
remote Driver selection = forbidden
```

这必须在 bevy_needle policy 层可审计。

Rig `ToolAccess` 可以作为辅助：

```text
限制工具
限制输出 effect
```

但不能成为“网络不会离开机器”的唯一证明。

---

# 12. confidence gate

## 12.1 业务语义

Needle3 返回：

```text
confidence
function_calls
validation / suppressed_calls（若有）
```

confidence gate 的要求：

```text
confidence >= threshold
    → 允许工具 turn

confidence < threshold
    → ToolInvocation == 0
    → Run enters escalation / confirmation
```

## 12.2 Rig 侧 Intercept

Rig 0.44 确实提供：

```text
Intercept
Decision
Verdict
Layer
```

它们是重要的候选 policy hook。当前文档将 Intercept 定义为可在 handler 前后施加策略的层。 

但是：

> **不要在规范里预先宣称“Intercept 是唯一落点”。**

第一轮 POC 必须验证：

```text
能否保证
model answer → tool materialisation
之间存在一个可访问 confidence 的拒绝点
```

如果 `Intercept` 无法自然拿到 bevy_needle 的 `EscalationPolicy` / Run / confidence，那么允许使用一个薄 bridge system。

## 12.3 禁止伪 gate

不能：

```text
已经 materialise ToolInvocation
        ↓
再检查 confidence
```

这已经太晚。

必须证明：

```text
low confidence
        ↓
zero tool execution
```

---

# 13. Cancellation / timeout / epoch

## 13.1 job envelope

每一个异步工作至少带：

```rust
struct JobEnvelope {
    run: Entity,
    epoch: u64,
    attempt: u32,
    driver_id: DriverId,
}
```

具体字段可按已有 `EscalationState` 合并，但 `(run, epoch)` 是不可省略的。

## 13.2 取消

```text
CancelRun
  ↓
RunStatus::Cancelled
  ↓
epoch += 1 / epoch invalidated
  ↓
old work may still finish
  ↓
old result returns
  ↓
epoch mismatch
  ↓
discard
```

禁止旧结果：

```text
resurrect Run
commit tool result
replace final state
```

## 13.3 timeout

单 tier timeout：

```text
tier attempt timed out
    ↓
mark current attempt failed
    ↓
if tier remains → next tier
else → Failed
```

总预算耗尽：

```text
direct terminal
```

不要继续盲目升级。

---

# 14. Driver identity 与多模型

这是当前实现最优先的真实 bug 类别。

## 14.1 禁止固定 ID

错误：

```rust
const RIG_DRIVER_ID: DriverId = DriverId("rig");
fn id(&self) -> DriverId { RIG_DRIVER_ID }
```

必须：

```rust
fn id(&self) -> DriverId { self.id }
```

## 14.2 capability 也必须实例化

错误：

```rust
fn capability(&self) -> Capability {
    Local
}
```

必须：

```rust
fn capability(&self) -> Capability {
    self.capability
}
```

## 14.3 state recognition 必须按具体模型隔离

不要：

```rust
esc.driver_id == "rig"
```

应该使用 per-model driver identity tracking：

```text
RigDriverIds<M>
```

并且：

```text
一个 M → 一个 inbox + 一个 step system
一个 DriverId → 一个 Driver
一个 Driver → 可绑定多个 tier
```

---

# 15. Async worker 设计

## 15.1 不允许 Driver 持 Future

Driver trait：

```text
submit
cancel
```

不允许：

```text
poll
Future
Waker
Runtime
```

## 15.2 推荐 channel 结构

```text
                ┌─────────────────┐
                │   Rig Driver    │
                └───────┬─────────┘
                        │ Job
                        ▼
                 worker channel
                        │
                        ▼
                dedicated worker
                        │
                blocking inference
                        │
                        ▼
                 result channel
                        │
                        ▼
                  ECS drain system
```

## 15.3 worker 回执纪律

以下都必须回执：

```text
completion success
completion error
worker panic / transport failure
channel closed
unmatched response
cancellation acknowledgement（若支持）
```

禁止：

```text
send failed → silently return
```

否则 Run 会永久停在等待态。

---

# 16. rig-ecs schedule 交界

这是 POC 的硬门之一。

当前 rig-ecs 拥有自己的 schedule，而 bevy_needle 也拥有自己的 system set。

必须建立明确帧序关系：

```text
Frame N
──────────────
RunPreparation
Needle completion drain
RunResolution
Driver submission

RigSchedule
──────────────
model / effect / handler processing

Frame N+1
──────────────
DriverEvent drain
RunResolution
```

## 16.1 必须验证

1. `finalize_escalations` 永远不会抢在 Rig handoff 前执行。
2. Rig model completion 必须最终进入 DriverEvent。
3. Tool effect 必须最终回到现有 ToolInvocation pipeline。
4. app shutdown 时两个 schedule 不会产生 dangling task。

## 16.2 禁止依赖“碰巧先后”

不得依赖：

```text
plugin add order
system registration order
Entity creation order
```

必须使用明确 `.before/.after` 或上游提供的 schedule contract。

---

# 17. POC 闸门

在任何旧 runtime 退休之前，必须通过全部 gate。

## G0 — API 基线

```text
cargo metadata
cargo tree
cargo doc
```

确认：

```text
rig-core = intended 0.44
rig-ecs  = intended 0.44
无 rig-run 旧前提
无旧 CompletionModel implementation
```

失败：**停止实现，不修改旧路径。**

## G1 — Needle3 model adapter

```text
Input
 ↓
Needle3 worker
 ↓
Rig completion-compatible response
```

验收：

- unary completion 成功
- provider raw 数据保留
- confidence 可取
- 错误有 typed failure
- 无主线程 block_on

## G2 — tool bridge

```text
Rig CallTools
 ↓
ToolInvocation
 ↓
ECS execution
 ↓
Tool result
 ↓
Rig
```

验收：

- 不重复注册工具
- call_id 原样往返
- pre-resolved 不执行 ECS
- 乱序完成按原 call order 回喂

## G3 — confidence gate

测试：

```text
confidence high
 → tool executes

confidence low
 → tool execution count == 0
 → Escalation path starts
```

## G4 — privacy gate

测试：

```text
LocalModelOnly
 + remote Driver registered
 = remote Driver 不可被选中
```

不能靠“默认没注册 remote”作为唯一测试。

## G5 — cancellation

至少覆盖：

```text
cancel during model
cancel during tool
cancel during child async job
```

所有迟到结果必须被 epoch 丢弃。

## G6 — concurrency

至少覆盖：

```text
Run A + Run B
same model type
same model instance
```

以及：

```text
Run A + Run B
same model type
不同 DriverId
```

和：

```text
不同 model type
不同 DriverId
```

验证 history 与 execution identity 都不串。

## G7 — schedule

至少连续跑 1000 次：

```text
Needle → Escalate → Rig → Tool → Result
```

不允许出现：

```text
permanent Escalating
lost event
duplicate tool execution
stale result commit
```

## G8 — fallback / rollback

关闭 `rig-ecs` feature 后：

```text
bevy_needle core
```

必须保持原有行为。

POC 任一 gate 失败：

```text
继续使用现有 Driver / Needle runtime
```

而不是把旧实现删掉后修新实现。

---

# 18. 测试矩阵

| 场景 | 期望 |
|---|---|
| Needle 高 confidence | 正常执行 |
| Needle 低 confidence | Escalating；ToolInvocation = 0 |
| tier 0 Rig 成功 | Completed / tool loop 正常 |
| tier 0 Rig 失败，tier 1 存在 | epoch +1，进入 next tier |
| 同 tier retry | tier 不变、attempt +1 |
| policy 禁止继续 | Escalated |
| 最后一档执行后失败 | Failed |
| 用户取消 | Cancelled |
| stale epoch | 忽略，不修改 Run |
| worker channel closed | DriverUnavailable/TransportError，最终收束 |
| call_id 缺失 | ProtocolError，不 mint |
| call_id 重复 | ProtocolError |
| preresolved | ECS execution count = 0 |
| A/B/C tool 乱序完成 | 回喂 A/B/C |
| 两个 Local tier | 两档均可命中 |
| 两个 Driver 相同 M | identity 不串 |
| 两个不同 M | inbox/system/history 不串 |
| LocalModelOnly + Remote registered | Remote 不可选 |
| rig feature off | core 编译且旧路径工作 |
| app shutdown | no leaked runtime / no silent hang |

---

# 19. 代码修改范围

## 19.1 第一阶段：只加，不删

新增：

```text
src/rig/
├── mod.rs
├── adapter.rs
├── model.rs
├── worker.rs
└── tool_bridge.rs
```

具体文件名可以根据现有 repo 结构调整，但职责不得混合。

### `model.rs`

只负责：

```text
Rig request → Needle request
Needle response → Rig response
```

### `worker.rs`

只负责：

```text
blocking native inference
session serialization
request/response channel
```

### `tool_bridge.rs`

只负责：

```text
Rig PendingToolCall
→ Bevy ToolInvocation
→ ToolResult
```

### 禁止

```text
model.rs
    创建 tokio runtime
    修改 World
    决定 escalation
```

```text
worker.rs
    修改 RunStatus
    调用 Bevy Tool
```

```text
policy.rs
    创建模型
    持 Arc<Model>
    创建 runtime
```

---

# 20. `RigDriver<M>` 处置策略

现阶段：**保留。**

原因：

1. 已经是现有 PR-B 的产品契约；
2. DriverId / capability / tier / epoch 已经围绕它形成测试；
3. POC 需要一个稳定边界来验证 Rig；
4. 立即删除会把“架构重构”和“功能验证”混成一次 breaking change。

但是：

> **冻结 runtime scope。**

从现在开始禁止往 `RigDriver` 增加：

```text
Agent scheduler
Tool registry
Memory engine
Checkpoint engine
Replay engine
Effect bus
```

它只负责“把 DriverAttempt 接到 Rig”。

---

# 21. 旧 runtime 退休条件

只有满足全部条件才允许删除：

```text
RigModelInbox<M>
rig_step_system<M>
ModelJob（若已被 Rig 自身 effect runtime 完全替代）
手工 AgentRun 步进
RigDriverState（若只剩冗余）
```

并且：

```text
现有测试矩阵等价通过

46 / 32 / 28
↓
全部重放
↓
不变量无回归
```

删除顺序：

```text
先新增 adapter
 ↓
POC
 ↓
双路径并行
 ↓
等价测试
 ↓
弃用标记
 ↓
删除旧 runtime
```

禁止：

```text
先删
 ↓
再发现 Rig 不可用
```

---

# 22. 不可接受的实现模式

```text
❌ ECS system block_on
❌ Driver 持 Future / Waker / Runtime
❌ Rig callback 直接 &mut World
❌ Rig callback 直接调用 ToolHandlerFn
❌ Rig 单独维护第二份 ToolRegistry
❌ 低 confidence 后才 materialise ToolInvocation
❌ call_id 缺失时静默 mint
❌ preresolved result 仍执行 ECS
❌ 按 ECS completion order 回喂 tool result
❌ 按 Entity id 生成 history
❌ 按 HashMap 顺序生成 history
❌ RemoteModel 可在 LocalModelOnly 下被选中
❌ ToolAccess 被当作“网络隔离证明”
❌ `Driver::id()` 固定为 "rig"
❌ `Driver::capability()` 固定 Local
❌ ensure_states() 根据固定字符串认领 Driver
❌ register_for_tier() 每绑定一次就重新 register Driver
❌ 同一个 M 注册多个 step system
❌ 不同 M 共用一个 inbox
❌ App shutdown 静默 drop channel
❌ worker 无回执
❌ epoch mismatch 仍修改 Run
❌ 为了复用 Needle 串行队列而全局串行化 Rig
❌ 默认 feature 引入 rig-core / rig-ecs / tokio / reqwest
❌ 使用已删除的 rig-run 作为当前实现前提
❌ 使用旧 CompletionModel API 实现 Rig 0.44 adapter
❌ 未验证 0.44 API 就按历史文档猜签名
```

---

# 23. 实施顺序

```text
Phase 0
  锁 Rig 0.44 + Cargo.lock
  更新旧 API 文档

Phase 1
  Needle3 worker facade
  不接 rig-ecs

Phase 2
  Rig 0.44 model/serve adapter POC
  unary first

Phase 3
  tool bridge
  call_id / pre-resolved / ordering

Phase 4
  confidence gate
  privacy gate
  cancellation / epoch

Phase 5
  rig-ecs schedule integration
  concurrency tests

Phase 6
  完整 POC 验收

Phase 7
  双路径长期运行

Phase 8
  决定是否退休旧 Rig runtime
```

---

# 24. Claude Code 执行纪律

Claude Code 在任何代码修改前必须：

1. `cargo metadata --locked`；
2. `cargo tree -e normal --locked`；
3. `rg` 当前版本源码确认 API；
4. 阅读当前 `rig-core` 0.44 的 `serve` / `driver` / `wire`；
5. 阅读当前 `rig-ecs` 0.44 的 `bus` / `agent` / `policy` / `systems`；
6. 阅读项目现有 `rig/driver.rs`、`rig/agent.rs`、`rig/transport.rs`；
7. 先写红测，再修改；
8. 每完成一层就 `cargo check` + 对应测试；
9. 不得因为“更现代/更简单”擅自删除已发布契约；
10. 未通过 POC 不得退休现有 runtime。

### 24.1 必须以本地源码为准

如果以下资料冲突：

```text
README
docs.rs landing page
旧版 Appendix
博客/Issue 评论
```

优先级：

```text
当前 pinned source
    > Cargo.lock
    > 当前 rustdoc
    > 当前 README
    > 历史 Appendix
```

---

# 25. 最终决策

## 保留

```text
EscalationPolicy
EscalationTarget
OnlineFallback
RunStatus
RunEscalation
Driver
DriverId
DriverRegistry
epoch
attempt
Needle native worker
ToolInvocation
```

## 新增/加强

```text
Rig 0.44 adapter
Needle ↔ Rig tool bridge
policy gate
privacy gate
schedule reconciliation
POC coverage
```

## 冻结但不删除

```text
RigDriver<M>
```

其内部 runtime scope 不再扩大。

## POC 后才允许退休

```text
RigModelInbox<M>
rig_step_system<M>
手工 AgentRun step loop
重复的 Rig async scheduler
```

---

# 26. 最终验收图

```text
                         User
                           │
                           ▼
                    Bevy Application Run
                           │
                    ┌──────┴───────┐
                    │              │
                    ▼              ▼
               Needle path     Escalation path
                    │              │
                    │        bevy_needle Policy
                    │        ├── confidence
                    │        ├── capability
                    │        ├── privacy
                    │        └── tier
                    │              │
                    │              ▼
                    │        Rig adapter
                    │              │
                    │              ▼
                    │         rig-ecs / Rig
                    │              │
                    │       ┌──────┴──────┐
                    │       ▼             ▼
                    │    Model         Tool Effects
                    │                      │
                    └──────────────┬───────┘
                                   ▼
                              ToolInvocation
                                   │
                                   ▼
                                Bevy ECS
                                   │
                                   ▼
                                  AuK
```

最终原则只有一句：

> **bevy_needle 决定“谁可以执行、为什么升级、能不能离机”；Rig 决定“这一轮 effect 怎么运行”；Bevy 决定“世界是什么”；AuK 决定“音频怎么被真正修改”。**

只有在这个边界通过 POC 后，才允许进一步精简 runtime。

---

# Appendix A：本轮外部核验基线

- Rig 0.44.0 当前公开文档确认存在 `rig_core::serve::{Serve, Layer, Intercept, Decision, Verdict}` 与 `serve::adapters::ModelAdapter`。
- Rig 0.44.0 当前核心 model 文档采用 `Model` / `DynModel` / `driver` / `operation` 体系。
- Rig 当前源码说明 `ModelAdapter` 会把 model 作为 serve/handler 注册；agent builder 也通过 `ModelAdapter` 安装 model route。
- rig-ecs 0.44.0 当前 docs.rs 定位为 “effects as entities, handlers as entities, the driver as a system”，默认无 feature，仅有 `assets` feature。
- Rig 当前官方 README 明确警告未来会继续发生 breaking changes，因此本项目必须 pin 并以本地源码为 API 真源。

这些事实只用于确定 API 边界；实现时仍必须通过本地编译验证具体 signature。

---

# Appendix B：与 v22 的关键变化

| v22 | v23 |
|---|---|
| `CompletionModel` 作为实现目标 | **改为 Rig 0.44 的 `Model/DynModel + Wire/Decoder/ModelAdapter` 时代 API；旧 CompletionModel 禁用** |
| `Intercept` 被描述为唯一 confidence gate | **改为候选 hook，POC 验证；唯一硬要求是 tool materialisation 前必须可拒绝** |
| `ToolAccess` 与 capability ceiling 强绑定 | **ToolAccess = tool permission；LocalModelOnly 仍由 bevy_needle policy 强制** |
| I13 仍写 rig-run | **删除当前实现前提；以当前 Rig 0.44 dependency graph 为准** |
| `RigDriver<M>` 可能继续扩大 | **明确冻结 runtime scope，只保留 adapter boundary** |
| rig-ecs 迁移可在 POC 后直接进行 | **保留双路径；POC 失败即可回滚，不得先删除旧 runtime** |

---

# Appendix C：参考来源

1. Rig 0.44.0 `rig-core` rustdoc：`serve`, `Model`, `DynModel`, `Wire`, `Decoder`。
2. Rig 0.44.0 `rig-ecs` rustdoc / CONTRACT：ECS effects, handlers, `UsesModel`, `ToolAccess`, scheduling and checkpoint semantics。
3. Rig 官方仓库 README / current source：runtime separation、model adapter 与快速迭代警告。
4. bevy_needle v22 执行规格：已发布 escalation contract、Driver contract、epoch/cancellation、ToolInvocation bridge、POC gates。

---

# Appendix D：提交前 Checklist

```text
[ ] v23 文档中的所有 0.42/0.43/旧 CompletionModel API 已明确标记为历史
[ ] Cargo.lock 锁定 Rig 版本
[ ] default build 无 rig dependency
[ ] Needle worker 可独立运行
[ ] Rig adapter unary POC 通过
[ ] Tool bridge 通过
[ ] confidence gate 通过
[ ] LocalModelOnly 通过
[ ] call_id strict round-trip 通过
[ ] pre-resolved 不执行
[ ] tool results 原序回喂
[ ] stale epoch 不落地
[ ] cancellation 不复活 Run
[ ] 同 M 多 DriverId 不串
[ ] 不同 M 不串
[ ] schedule order 有自动化测试
[ ] app shutdown 无悬挂
[ ] POC 失败仍可运行旧路径
[ ] 未通过前未删除旧 runtime
```
