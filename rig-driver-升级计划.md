# bevy_needle PR-B 执行规格：Driver 契约与 rig 适配

> 版本：**v5**（2026-09-08）· 面向 Claude Code 执行
> 前序版本 v1–v4 的事实考据**全部保留**，已移至 Appendix，未删除。
>
> **v4 → v5 的结构变化**
> - 证据与契约分层：§0–§10 是可执行契约，考据移入 Appendix A/B/C
> - **`Driver` 从隐含概念升为第一等架构对象**（新增 §4）
> - 新增 §1「不可协商不变量」15 条、**新增 §11「禁止实现模式」**（原建议 18）
> - 新增 §7 错误/取消/超时行为表、§8 状态机测试矩阵
> - 开放问题按 P0–P3 分级（原建议 17）
> - 采纳两条语义澄清：**Driver ≠ Tool ≠ NeedleBackend ≠ Agent**；**Bevy Run ⊃ AgentRun**（原建议 1、7）
> - 采纳 `EscalationState` 与 `RunStatus` 分离、**限制 `finalize_escalations` 为安全阀**（原建议 10、11）
> - 采纳 feature 拆分 `escalate`（能力）/ `rig`（实现）（原建议 15）
> - 采纳「tokio 问题降级为依赖维护任务，不进架构」（原建议 16）

---

## §0 PR-B Mission

```
让 bevy_needle 在「置信度不足」时，能把同一次 Run 的推理责任 handoff 给另一个 Driver，
而 crate 核心永远不知道 rig 是什么。

注意措辞：不是「重做」。Driver 接手后可能继续、扩展上下文、重新规划或提高能力，
不必然重新生成完整 prompt。实现者不得据此假设「必须重放原始 prompt」。

0.2.0 已完成：契约面（EscalationPolicy / EscalationTarget / LazyModel /
   RunStatus::Escalating{tier} / RunStatus::Escalated / finalize_escalations）。
PR-B 只补三样：Driver 抽象、DriverCoordinator、RigDriver 适配。
另有两项 breaking 变更随 PR-B 一次性完成（§2.3）。

成功判据：
  - 默认构建仍无 rig / tokio / reqwest
  - MockDriver 能跑通完整升级链路（无需网络、无需 rig）
  - rig 的 tool call 必定走 ECS ToolInvocation pipeline
```

---

## §1 不可协商不变量（22 条）

违反任何一条即 PR 不合格。

| # | 不变量 |
|---|---|
| **I1** | **主线程永不 `block_on`**。任何 Driver 都不得在 ECS system 内执行 async model call |
| **I2** | **Driver 不是 ToolHandler，不是 NeedleBackend，不是 Agent**。Driver = 「如何取得一次 Run 推理结果」的策略实现，与工具系统完全正交 |
| **I3** | **工具执行必须在 ECS 侧**。rig 的 `CallTools` 必须翻译成 `ToolInvocation` 实体，不得在 rig async 回调里直接调 `ToolHandlerFn` |
| **I4** | **Bevy Run ⊃ AgentRun**。`AgentRun` 只是**一次 Driver attempt 的内部状态**，不是 Run 本身。**绝不让 agent runtime 反过来成为应用 runtime** |
| **I5** | **Policy 是纯决策，Driver 是纯执行**。`Driver` 不得内含 `EscalationPolicy`、不得自己决定升不升档；**Policy 不得持有模型 / client / runtime** |
| **I6** | **`RunResolutionSystems` 不得 match 具体 provider**。它只认识 `DriverId`，不认识 `Rig` / `OpenAI` / `Candle` |
| **I7** | **模型构造能力属于 `DriverRegistry`，不属于 `EscalationTarget`**。Run 只持 `tier` / `driver_id` / `attempt` / `epoch`，不持 `Box<dyn Model>` |
| **I8** | **`RunStatus` 只表示生命周期，不承载上下文**。上下文放 `EscalationState` 组件 |
| **I9** | **`finalize_escalations` 是安全阀，不是业务逻辑**。它只保证「不悬挂」，不决定用哪个 driver、重试几次 |
| **I10** | **tier 是 `EscalationPolicy.tiers` 的下标，不是 capability rank**。**禁止** `tier = max(rank())`。单调性只保证「tier 下标不回退」（见 §4.1 反例） |
| **I11** | **默认构建零成本**：`cargo tree` 无 rig-* / tokio / reqwest，进 CI 断言 |
| **I12** | **MSRV 保持 1.98**，不得回退 |
| **I13** | **依赖 rig-run，不依赖 rig-agent**。理由不是 tokio，而是 **runtime ownership**：rig-agent 拥有 agent loop / 工具执行生命周期 / runner / memory 集成，而我们要的只是 `AgentRun` 这个 protocol state machine |
| **I14** | **转录必须按 `ChatMessageSeq` 排序**，不得用 Entity id 或 HashMap 顺序构建 rig history |
| **I15** | **PR-B 必须复用 0.2.0 已固化的契约面**，不得另起一套档位表示（§2.3 的两项 breaking 除外，那是修形状不是另起） |
| **I16** | **Driver trait 不得暴露异步推进接口（无 `poll`）**。异步结果经 channel 回灌，ECS 只 drain。**Driver 不得持有 Future / waker / runtime** |
| **I17** | **每个异步 job 必须携带 `(run, epoch)`**。epoch 不匹配的结果**丢弃**，不得修改 Run |
| **I18** | **工具 call_id 严格往返**。缺失即 `DriverProtocolError::MissingToolCallId`，**禁止 mint 新 ID** |
| **I19** | **`preresolved_result` 非空时不得进 ECS 执行**，直接回灌 |
| **I20** | **工具结果必须按原始 call order 重排后回喂**，不得依赖 ECS 完成顺序 / Entity id / HashMap 顺序 |
| **I21** | **Needle 与 Rig 是不同的并发域**。不得为复用 needle 串行队列把 Rig 全部串行化 |
| **I22** | **能 `block_on()` 的类型只存在于 worker 侧**。优先类型系统隔离，其次 worker-private API，文档约束是最后防线 |

---

## §2 当前架构（0.2.0 已发布，实测）

```
RunAgent 消息
    ↓
RunPreparationSystems   capture_run_requests → run 实体
    ↓
RunExecutionSystems     execute_needle_runs（引擎在独立工作线程）
    ↓
RunResolutionSystems    resolve_run_tool_turns
                        finalize_escalations ← 安全阀
    ↓
ToolDispatchSystems     dispatch_registered_tool_calls（ECS 侧）
    ↓
RunCommitSystems        终态落入会话转录
```

**0.2.0 已固化的契约面（PR-B 不得改形状）**

```rust
// policy.rs —— 无 cfg、无 rig 依赖
pub struct EscalationPolicy {
    pub enabled: bool,                      // false 时仍走 Escalating→Escalated，无外部动作
    pub fallback: OnlineFallback,           // Never（默认）/ LocalModelOnly / Cloud
    pub tiers: Vec<EscalationTarget>,       // 按 rank 升序；索引即 RunStatus::Escalating.tier
    pub confidence_threshold: Option<..>,   // None = 用各 agent 的门限
    pub total_budget: Duration,
    pub per_tier_timeout: Duration,
}
pub enum EscalationTarget { Needles, Local(LazyModel), Remote(LazyModel) }
impl EscalationTarget { pub fn rank(&self) -> u8 }   // 0 / 1 / 2
pub enum OnlineFallback { Never, LocalModelOnly, Cloud }
pub type LazyModel = Arc<Box<..> + Send + Sync>;     // ⚠️ 泛型被折叠，见 Q26

// run.rs
pub enum RunStatus {
    Queued, Running, Completed,
    Escalating { tier: u32 },   // 只有 tier，不承载上下文（I8）
    Escalated,                  // 终态，不是失败
    Failed,                     // 已净化：「置信度门控不进入此状态（见 Escalated）」
    Cancelled,
}
pub struct RunEscalation { run: Entity, confidence: f64, threshold: f32 }
mark_run_escalating / mark_run_escalated / persist_escalated_runs

// app.rs
pub fn finalize_escalations(world: &mut World)  // 「未来 rig driver 在此状态上接管」
```

**版本事实（三处区分，消除歧义）**

| 来源 | 版本 | 核验时点 |
|---|---|---|
| crates.io 已发布 | **0.2.0**，"about 2 hours ago" | 2026-09-08 |
| docs.rs 当前索引 | **0.2.0**，首页标题/面包屑/依赖声明一致 | 2026-09-08 |
| 本地 git HEAD | 待维护者确认 | — |

> ⚠️ 有第三方反馈称 docs.rs 显示 0.1.0。**经复检该说法不成立**（docs.rs 版本页明确列出 `0.2.0 (2026-09-08)` 与 `0.1.0 (2026-09-02)`，无更新版本）。若你本地看到的不是 0.2.0，是缓存问题，执行前先 `cargo update -p bevy_needle`。

### 2.3 PR-B 内一次性完成的两项 breaking 变更

**推翻 I15 的字面约束一次，理由是修形状而非另起一套。**

`EscalationTarget` 当前形状 `Needles / Local(LazyModel) / Remote(LazyModel)` 有两个缺陷：

1. **泄漏 execution**（违反 I5/I7）：Policy → `EscalationTarget` → `LazyModel` → Rig/OpenAI/Candle，policy 最终又知道执行。正确链路应是 Policy 只说「我要 Local」，由 `DriverCoordinator` 查 `DriverRegistry` 决定 Local 对应哪个 Driver。
2. **拼写**：`Needles` 复数与 `rank()` 文档的 `0=Needle` 单数不一致。

**改为**：

```rust
pub enum EscalationTarget { Needle, Local, Remote }
// LazyModel 移出，归 DriverRegistry 持有
```

**为什么现在改而不是以后改**：

- 0.2.0 里 `Local`/`Remote` 的载函位**当前不可实现**（文档明确「当前阶段只实现 `Needles`」）——它是预留扩展点，不是已交付能力，破坏面近乎为零
- `escalate` feature **尚未发布**，没有任何下游依赖 `Local(LazyModel)` 的构造形式
- 两个改动（去载函 + 改拼写）落在同一个 enum 上，**必须一次做完**，否则付两次 breaking 成本
- 越晚改越贵：等 `escalate` 发布后，改这个 enum 会同时破坏 policy 配置代码与所有示例

**保留 `rank()`**，但它的语义收窄为 **capability 序**（用于 fallback 上限判断），**不再用于推导 tier**（见 I10 / §4.1）。

---

## §3 目标架构

```
                      ┌──────────────────┐
                      │ EscalationPolicy │  纯决策（I5）
                      └────────┬─────────┘
                               │ target
                               ▼
Run ──► Escalating{tier} ──► DriverCoordinator ──► DriverRegistry
                               │  只认识 DriverId（I6）    └─ 持有 LazyModel（I7）
              ┌────────────────┼────────────────┐
              ▼                ▼                ▼
        NeedleDriver      RigDriver        FutureDriver
                               │
                               ▼
                          rig::AgentRun    ← 一次 attempt 的内部状态（I4）
                               │
                      CallModel / CallTools
                               │
                    ┌──────────┴──────────┐
                    ▼                     ▼
              Model async          ECS ToolCall       ← 强制（I3）
              （worker 侧）                 │
                    │                     │
                    └──────────┬──────────┘
                               ▼
                    DriverEvent channel（I16）
                               │
                               ▼
                    ECS drain（无 poll）
                               │
                               ▼
                        RunResolution
                               │
                    ┌──────────┴──────────┐
                    ▼                     ▼
              DriverResult            （driver 未接管
                    │                  → finalize_escalations
        ┌───────────┼───────────┐        → Escalated）
        ▼           ▼           ▼
   Completed   NeedEscalation  Failed
              （next tier）
```

**`Escalating` 是中间态不是结果** —— 它出现在 `RunResolution` 的**输入侧**（Run 被判定需升级），不出现在 DriverResult 的输出侧。输出侧只有 `Completed` / `NeedEscalation(next tier)` / `Failed`。

**三条接线约定（由 0.2.0 现状倒推，非自由设计）**

1. **PR-B 的系统必须排在 `finalize_escalations` 之前** —— 后者会把所有 `Escalating` 收束为 `Escalated`，排在其后则永远等不到接管
2. **档位索引直接复用 `EscalationPolicy.tiers`** —— 文档已定「索引即 `Escalating.tier`」，取 `tiers[tier]` 即可；**tier 是下标，不是 rank**（I10）
3. ~~`Local` / `Remote` 的 `LazyModel` 载函位已预置~~ → **已推翻，见 §2.3**。改由 `DriverRegistry` 持有模型构造能力

---

## §4 Driver 契约（PR-B 的核心新增）

### 4.1 分层职责与 tier/rank 反例

```
EscalationPolicy  决定「是否升级、升到哪个 tier」   ← 0.2.0 已有
EscalationTarget  描述「目标能力类别」              ← 0.2.0 已有（PR-B 去掉载函，§2.3）
DriverRegistry    负责「tier → 哪个 Driver」        ← PR-B 新增，持有 LazyModel（I7）
Driver            负责「怎么执行」                   ← PR-B 新增
Model             真正产生推理结果                   ← 由 DriverRegistry 构造，Driver 使用
```

**为什么 `tier ≠ rank`（I10 的反例，必读）**：

```
policy.tiers = [Local(Candle), Local(GPTQ), Remote]
capability rank:  Local(Candle)=1  Local(GPTQ)=1  Remote=2
policy tier:            0                1            2
```

若按 `tier = max(rank())` 推导，Candle 与 GPTQ 的 rank 相同 → tier 坍缩，第二个 Local 档永远选不中。
**正解**：tier 就是 `EscalationPolicy.tiers` 的**数组下标**，单调递增指「下标只增不减」。`rank()` 只用于 capability 上限判断（如 `fallback = LocalModelOnly` 时拒绝 Remote）。

### 4.2 trait 草案（**无 poll**，I16）

```rust
/// 一次 Run 推理结果的取得策略。与工具系统正交（I2）。
pub trait Driver: Send + Sync + 'static {
    fn id(&self) -> DriverId;

    /// 提交一次 attempt。立即返回，不阻塞（I1）。
    /// 结果经 DriverEvent channel 回灌 —— 不经过返回值，不经过 poll。
    fn submit(&self, ctx: DriverAttemptCtx) -> Result<AttemptHandle, DriverError>;

    /// 协作式取消。不保证底层 model 中断（candle load 不可中止）。
    fn cancel(&self, handle: AttemptHandle);
}
```

**为什么删除 `poll()`**：

`poll(&mut AttemptHandle) -> Option<DriverEvent>` 会强迫 Driver 自己保存 Future、管理 waker、感知 runtime —— 等于**在 Driver 里重造一个简化版 async runtime**，且异步细节泄漏给 ECS 主循环。这与 §13「Tokio 边界集中在 worker」直接冲突，二者只能留一个。

**正确形态**：`submit` → worker spawn → channel → ECS `drain_events(Res<DriverEventReceiver>)`。ECS 只消费，不推进。

> **但不要误解为「poll 模式一律禁止」**：rig #2446 的 `Pending::poll_outcome` 是**对 sans-I/O 状态机的 poll**，不是对 async future 的 poll。若未来采用 rig-bus，它是 **transport 适配层的实现细节**，仍然不是 `Driver` trait 的契约面。契约面只保留 `submit` / `cancel`。

### 4.3 所有权与生命周期（I4 / I7）

```
Run  (Entity)
  └─ RunStatus::Escalating { tier }    ← 只表示生命周期（I8）
  └─ EscalationState { .. }            ← 全部上下文在此，见下
        └─ 不持有 Box<dyn Model>

DriverRegistry (Resource)
  └─ tier → DriverId 映射
  └─ 每个 Driver 一份 LazyModel（全局缓存，见下）
        └─ rig runtime / model
```

**`LazyModel` 生命周期决策：选「每 driver 一份，全局缓存」**

理由：Run 生命周期是帧级的、可取消的；模型初始化是秒级的、**不可取消**的（candle load 进 blocking pool 后 drop future 不停）。耦合会导致 run 取消时模型反复重建。

### 4.4 `EscalationState` 完整字段

```rust
pub struct EscalationState {
    pub tier: u32,              // EscalationPolicy.tiers 下标（I10）
    pub driver_id: DriverId,    // 当前执行者（I6）
    pub epoch: u64,             // 防 stale event（I17）—— 必须落进数据结构，不能只是文档约束
    pub attempt: u32,           // 当前 tier 内的第几次尝试
    pub deadline: Instant,      // per_tier_timeout 的到期时刻
    pub reason: EscalationReason, // 为什么升级（低置信度 / 引擎错误 / 解析失败 / ..）
}
```

**`tier` 与 `attempt` 必须分开**（不要 attempt 隐含在 tier 里）：

```
Run
 └─ EscalationState
      ├─ tier = 1
      └─ attempts: #1 → #2 → #3     ← 重试不增加 tier
```

否则「tier 1 重试 3 次」会被误表达成「tier 4」。

### 4.5 DriverAttempt 概念

```rust
pub struct DriverAttempt {
    pub run: Entity,
    pub tier: u32,
    pub attempt: u32,
    pub driver: DriverId,
    pub epoch: u64,
    pub status: DriverAttemptStatus,  // Pending / InFlight / Succeeded / Failed / Cancelled
    pub outcome: Option<AttemptOutcome>,  // 区分 init 失败 / 模型拒绝 / timeout / tool failure
}
```

第一版不一定要做成 Component，但**概念必须存在** —— 否则诊断时只看 `Escalating { tier: 2 }` 无法回答：升级过几次、哪个 driver 失败、是初始化失败还是超时。

### 4.6 DriverId 而非 enum

```rust
pub struct DriverId(&'static str);   // "needle" / "rig" / "human" / ..
```

用 id 不用 enum，是为了让 `RunResolutionSystems` 永远不需要 match 具体 provider（I6）。第三方 crate 可注册新 driver 而不改核心。

---

## §5 Rig 适配层

### 5.1 AgentRun 与 Bevy Run 的关系（I4）

```
Bevy Run #42
    └── RigDriverState
            └── rig::AgentRun   ← 只是一次 attempt 的内部状态
```

**绝不能 `Bevy Run == AgentRun`。** 一次 Bevy Run 可能经历：Needle attempt → Rig attempt #1 → Rig attempt #2 → Human approval。

### 5.2 工具桥（I3，整个集成最值得坚持的一点）

```
rig AgentRun
     │ CallTools
     ▼
bevy_needle ToolCall / ToolInvocation    ← 复用现有 ECS pipeline
     │
     ▼
ECS systems（dispatch_registered_tool_calls）
     │
     ▼
tool results
     │
     ▼
rig AgentRun::tool_results(..)
```

**不要** `rig → Tool callback → Bevy World`。0.2.0 已有 `ToolInvocation` 状态机与 `External` dispatch 路径，**没必要为 rig 再造一套工具执行抽象**。

三个必须处理的细节：
1. `PortableDynamicTool` 而非 `DynamicTool` —— 前者 context-free，rig 侧只持定义，ECS 侧按名解析，**让 I3 从「靠纪律」变成「类型上无法违反」**
2. `PendingToolCall::preresolved_result` 非空时**不进 ECS**，直接回灌（rig 的 invalid tool-call recovery）
3. `internal_call_id` / `tool_call.id` 必须原样往返到 `ToolCall.call_id`

### 5.3 转录

- 首次交接：needle 段是 ECS 实体，用 `collect_transcript`（按 `ChatMessageSeq`，I14）取快照
- tier 链内部：用 `ConversationMemory`（`Arc<M>` blanket impl，一个 bridge 挂多个 tier）
- `DemotingPolicyMemory` 的 watermark 是**必需品** —— 多 tier 各 load 一次时保证只 demote 一次
- `transcript::validate_canonical`（#2403）是上游自带的校验器，应对 `tool_call_id` 一致性

---

## §6 状态转移

```
Queued
  ↓
Running
  ↓
RunResolution  ──── 判定 ────┬── Completed
                             ├── Failed
                             ├── Cancelled
                             └── Escalating { tier }   ← 中间态，不是结果
                                      ↓
                              Driver[tier]（经 DriverRegistry 解析）
                                      ↓
                                 DriverResult
                                      ↓
                    ┌─────────────────┼─────────────────┐
                    ▼                 ▼                 ▼
              Completed        NeedEscalation        Failed
                            （tier+1，重新进入           │
                             Escalating）               │
                                                        ▼
                                              （无更多 tier / 无可用 driver）
                                                        │
                                          ┌─────────────┴─────────────┐
                                          ▼                           ▼
                                       Failed                    Escalated
                                    （试过且坏了）              （没试/不许试）
```

**终态归属判定规则（消除 `Escalated` / `Failed` / `Cancelled` 重叠）**

一句话判据：**「没试 / 不许试」→ `Escalated`；「试过且坏了」→ `Failed`；「用户喊停」→ `Cancelled`。**

| 情况 | 终态 |
|---|---|
| 无可用 Driver（`escalate` 关闭 / 无匹配注册） | `Escalated` |
| policy 禁止继续（`OnlineFallback::Never`、tier 上限） | `Escalated` |
| Driver 执行失败（协议错误 / 传输错误 / driver 崩溃） | `Failed` |
| 模型返回错误 | `Failed` |
| tier 耗尽且最后一档确实失败过 | `Failed` |
| 用户 / 系统取消 | `Cancelled` |

> 关键区分落在「最后一个 tier 到底执行过没有」：执行过且失败 → `Failed`；根本没执行（无 driver、policy 不许）→ `Escalated`。`Escalated` 的语义是「按契约选择了升级而不是执行」的**正常收尾**，不是失败。

**语义澄清：第一版只支持 Escalation of inference**（原建议 9）

```
✅ 支持的：
   模型回答 confidence < threshold
   → 不执行工具调用
   → 换更强 driver 重新推理

❌ 第一版不支持的：
   模型已决定 call dangerous_tool(..)
   → Bevy 拦截 → rig 继续
   （这会把状态机搞得极难维护）
```

---

## §7 错误 / 取消 / 超时 / 关闭行为表

| 场景 | 行为 | 落点 |
|---|---|---|
| 某 tier 成功 | 回 `Running`，结果落入转录 | `DriverEvent::Succeeded` |
| 某 tier 失败且还有档 | `Escalating { tier + 1 }`，**attempt 归零、epoch +1** | Coordinator |
| 同 tier 内重试 | `attempt + 1`，**tier 不变** | Coordinator |
| 档位耗尽且末档执行过 | `Failed`（带 `DriverAttempt` 链作诊断） | Coordinator |
| policy 禁止继续 / 无可用 driver | `Escalated`（未执行，正常收尾） | 安全阀 |
| 单档超时（`per_tier_timeout`） | 视为该档失败，进下一档 | Coordinator |
| 总预算超时（`total_budget`） | 直接终态，不再升档 | Coordinator |
| 升级途中被取消 | `Cancelled`，**epoch 失效** | `Driver::cancel` |
| **stale event（epoch 不匹配）** | **丢弃，不改 Run**（不是错误，是正常并发控制） | drain system |
| job queue 关闭 / sender dropped | 终态化，不静默悬挂 | 安全阀 |
| `NoSnapshot` | **直接 `Failed`**，不升级 | 开发者错误，升级救不了 |
| 工具 call_id 缺失 | `DriverProtocolError::MissingToolCallId`，**禁止 mint** | tool bridge |
| **App shutdown** | stop accepting → drain or discard → join worker → 释放 runtime | worker |

**错误分类（禁止全部压成 `Engine(..)`）**：

```
PolicyError          策略不允许 / 配置非法
DriverProtocolError  call_id 缺失、重复 ID、状态机非法转移   ← stale 不属于此类
DriverUnavailable    无匹配 driver / driver 未注册
TransportError       channel 断开、worker panic
ModelError           模型返回错误
ToolExecutionError   工具执行失败（可回灌）
StaleEvent           epoch 不匹配 —— 不是 Run failure，是正常并发控制
Cancelled            逻辑取消
```

**`finalize_escalations` 的职责边界（I9）**：

```
✅ 它做：  任何 escalation 在离开系统前必须变成
          「已提交下一 driver」/ Failed / Cancelled / Escalated
❌ 它不做：选哪个 driver、重试几次、是否用 rig、是否切模型
```

**取消是逻辑取消，不是强制中断**（candle load / 部分 provider 不可中断）：

```
CancelRun → 标记 Cancelled → epoch 失效 → 旧 work 可能跑完 → 结果回来 → epoch 不匹配 → 丢弃
```
**绝不允许旧响应复活 Run。** 没有 epoch 就无法可靠区分，这是 I17 存在的唯一理由。

---

## §8 测试矩阵（状态机级）

| 场景 | 期望 |
|---|---|
| confidence 足够 | 正常 tool execution |
| confidence 不足 | 进入 `Escalating` |
| tier 0 → driver | 提交 attempt |
| driver success | 回到 `Running` |
| rig tool call | 产生 ECS `ToolInvocation` |
| tool result 回灌 | `AgentRun::tool_results` 收到正确 call_id |
| **call_id 缺失** | `MissingToolCallId`，**不 mint** |
| **call_id 重复** | 拒绝 |
| **preresolved_result** | **ECS 执行次数 == 0** |
| **混合批次（A 普通 / B preresolved / C 普通）** | A、C 执行；B 不执行 |
| **多工具乱序完成（C→A→B）** | 回喂前重排为 A→B→C |
| rig Done | `Completed` |
| rig error | next tier / `Failed` |
| rig timeout | next tier / `Failed` |
| 同 tier 重试 3 次 | `attempt == 3`，**`tier` 不变** |
| **tier 0/1 同为 Local 不同模型** | 两个 tier 都能命中（验证 I10 反例） |
| 升级途中取消 | `Cancelled`，旧响应不复活 |
| **stale epoch 事件** | **不修改 Run** |
| queue 关闭 | 终态化，不悬挂 |
| **worker shutdown** | 无 runtime 泄漏、无 panic 静默 |
| **rig feature off** | core 正常编译 |
| **escalate on / rig off** | 能力层可编译可测（FakeDriver） |
| **default build** | 无 rig / tokio / reqwest |
| **多 run 并发** | **history 不串** |
| **同 run 多轮** | **history 顺序正确** |
| **Needle + Rig 并发** | 两个并发域互不阻塞 |

加粗项是本轮新增。最后两条最关键 —— rig 的 history **绝不能**通过 Entity 顺序或 HashMap 顺序构建（I14）。

---

## §9 依赖与 feature 契约

```toml
[features]
default  = ["dlopen"]
dlopen   = ["dep:libloading"]
escalate = []                    # 能力：状态机 + Driver 抽象（无 rig）
rig      = ["escalate", "dep:rig-run", "dep:rig-core", "dep:tokio"]
rig-local = ["rig", "rig-core?/ollama"]   # 弱依赖语法

[dependencies]
rig-core = { version = "0.42", default-features = false, optional = true }
rig-run  = { optional = true }   # ⚠️ 未发布，需 git pin
tokio    = { version = "1", features = ["rt-multi-thread"], optional = true }
```

**feature 分层理由（原建议 15）**：`escalate` = 能力，`rig` = 一种 driver 实现。Policy 与状态机在无 rig 时也存在。未来加 `human` / `remote` driver 不会让 `escalate` 语义膨胀。

**CI 断言**：

```bash
cargo check                        # 默认
cargo check --no-default-features
cargo tree  --no-default-features  # 断言：无 rig-core / tokio / reqwest
cargo check --features escalate   # 无 rig 也能编译（MockDriver 可测）
cargo check --features rig        # 断言：rig-core 出现
```

**tokio 问题的定位（原建议 16）**：「rig-core 何时移除 tokio」是 **Cargo 依赖维护任务，不是架构变更**。不要让上游排期进入 PR-B 设计。当前事实记在 Appendix C。

---

## §10 开放问题（P0–P3 分级）

**P0 —— 开工前必须确定（不确定则不动手）**

| # | 问题 |
|---|---|
| P0-1 | `finalize_escalations` 在 `RunResolutionSystems` 内的确切排序（决定 PR-B 能否接管） |
| P0-2 | ~~`Driver::poll()` 签名~~ → **已裁决：删除 poll，改 channel 回灌**（I16 / §4.2）。剩 `submit` / `cancel` 的错误类型待定 |
| P0-3 | Driver 与 Run 的生命周期边界（`AttemptHandle` 放 Resource 还是 Component） |
| P0-4 | rig tool call → ECS `ToolInvocation` 的字段映射，尤其 call_id 往返 |
| P0-5 | 升级重试时是否重建 `AgentRun`（建议：每次 attempt 重建，history 从 bridge 读） |
| **P0-6** | **§2.3 两项 breaking（`EscalationTarget` 去载函 + `Needles`→`Needle`）是否随 PR-B 一起发** —— 维护者决策，影响后续所有代码 |

**P1 —— PR-B 实现前必须确定**

| # | 问题 |
|---|---|
| P1-1 | ~~超时时间源~~ → **已裁决：进 `EscalationState.deadline`**（§4.4），不扩展 `RunStatus` 变体 |
| P1-2 | 取消语义：`Driver::cancel` 是协作式，candle load 不可中止的兜底 |
| P1-3 | 诊断面：`DriverAttempt` 是否做成 Component |
| P1-4 | `EscalationPolicy` 是 Resource 还是 Component（决定能否按 agent 定制） |
| P1-5 | `LazyModel` 的真实泛型参数（docs.rs 折叠了，见 Q26）—— 现归 `DriverRegistry`，需确认其构造签名 |
| P1-6 | `DriverRegistry` 的注册 API 形态（`tier → DriverId` 映射如何配置） |

**P2 —— 实现中可决定**

| # | 问题 |
|---|---|
| P2-1 | 流式 delta 的收割 API |
| P2-2 | `RunSpec` 能否 `Serialize` 以支持 run 存盘 |
| P2-3 | driver metrics / telemetry（填 `Telemetry` 预留位） |
| P2-4 | `ToolDispatchPolicy::External` 是否给 rig 路径用 |
| P2-5 | 是否允许 Rig → Needle 回退（建议：架构不阻止，policy 默认禁止） |

**P3 —— 后续版本**

| # | 问题 |
|---|---|
| P3-1 | Remote / Human driver |
| P3-2 | 动态 provider 加载 |
| P3-3 | 工具语义召回（rerank + `PortableToolEmbedding`） |
| P3-4 | `confidence: f64` / `threshold: f32` 统一为 f32（breaking） |

**f64/f32 wart 的处理（原建议 13）**：PR-B **不修改** `RunEscalation` 的字段类型（0.2.0 已发布，改是 breaking）。改为在 `EscalationPolicy` 上提供唯一比较入口：

```rust
impl EscalationPolicy {
    pub fn should_escalate(&self, confidence: f64) -> bool {
        confidence < self.threshold_or_default() as f64
    }
}
```
**cast 只允许出现在这里，不得散落在 systems 里。**

---

## §11 禁止实现模式（哪些看起来合理但绝对不能做）

```
❌ Driver 在 ECS system 里 block_on                    （违反 I1）
❌ rig callback 直接拿 &mut World                      （违反 I1/I3）
❌ rig Tool callback 直接执行 Bevy tool                （违反 I3）
❌ 为 rig 单独建立一套 ToolRegistry                    （0.2.0 已有，重复即错）
❌ Run 直接持有 AgentRun / Box<dyn Model>              （违反 I4/I7）
❌ EscalationPolicy 内部创建 tokio runtime             （违反 I5）
❌ 默认 feature 引入 rig-core                          （违反 I11）
❌ RunResolutionSystems 里 match RigDriver / OpenAI    （违反 I6）
❌ 用 Entity id 或 HashMap 顺序构建 rig history        （违反 I14）
❌ escalation 失败后仍保持 Escalating                  （违反 I9，会悬挂）
❌ Driver 内含 EscalationPolicy 自行决定升档           （违反 I5）
❌ 让 RunStatus 膨胀成 Escalating{tier,attempt,driver,error,..} （违反 I8）
❌ finalize_escalations 决定用哪个 driver / 重试几次   （违反 I9）
❌ 依赖 rig-agent 而非 rig-run                         （违反 I13）
❌ 把「rig 何时移除 tokio」写进架构前提                （依赖问题非架构问题）

── 本轮新增 ──────────────────────────────────────────────
❌ 给 Driver trait 加 poll() 推进 async                （违反 I16，等于重造 executor）
❌ Driver 持有 Future / waker / runtime                （违反 I16）
❌ ECS 直接 poll driver 而不走 channel                 （违反 I16）
❌ 异步 job 不携带 (run, epoch)                        （违反 I17）
❌ epoch 不匹配的事件仍修改 Run                        （违反 I17，会复活已取消的 run）
❌ epoch 只写在文档里而不落进 EscalationState          （违反 I17，约束会被绕开）
❌ call_id 缺失时 mint 新 ID                           （违反 I18）
❌ preresolved_result 非空仍进 ECS 执行                （违反 I19）
❌ 工具结果按 ECS 完成顺序直接回喂                     （违反 I20，必须重排）
❌ 为复用 needle 串行队列把 Rig 全部串行化             （违反 I21）
❌ 把 block_on() 做成 ECS 可调用的普通 pub API         （违反 I22）
❌ tier = max(rank()) 推导档位                         （违反 I10，同 rank 多档会坍缩）
❌ 把 attempt 隐含在 tier 里（重试就 tier+1）          （违反 §4.4）
❌ EscalationTarget 保留 LazyModel 载函                （违反 I7，policy 泄漏 execution）
❌ 把 Escalated 当失败用 / 把无 driver 当 Failed       （违反 §6 终态表）
```

> **一条容易误伤的边界**：`poll` 不是一律禁止。rig #2446 的 `Pending::poll_outcome` 是**对 sans-I/O 状态机的 poll**，合法且好用；禁止的是**对 async future 的 poll 出现在 `Driver` 契约面**。前者归 transport 适配层，后者归 worker。

---

## §12 一句话总结

**0.2.0 已把「升级」钉成契约，PR-B 只补「谁来接管」。** 核心是让 `Driver` 成为第一等架构对象：Policy 纯决策、Driver 纯执行、`RunResolutionSystems` 只认 `DriverId`、工具永远走 ECS。这样 `bevy_needle` 的核心**永远不知道 rig 是什么**，rig 只是一个可选 driver，未来加 OpenAI / Candle / Human 都不用改核心。

**本轮三条最关键修正**：
1. **`Driver` 无 `poll`** —— 异步推进归 worker，结果经 channel 回灌，ECS 只 drain。否则每个 Driver 都要重造一个简化版 executor
2. **`EscalationTarget` 去掉 `LazyModel`** —— 模型构造能力归 `DriverRegistry`，Policy 才真正是纯决策（I5/I7）
3. **tier 是下标不是 rank** —— `[Local(Candle), Local(GPTQ), Remote]` 里两个 Local 的 rank 相同，用 rank 推 tier 会让第二个档永远选不中

**以及最重要的一句话（保持不动）**：**Bevy Run ⊃ AgentRun**。绝不让 agent runtime 反过来成为应用 runtime —— 这是 AI agent 项目最常见的错误，对 ECS 框架尤其致命。

---
---

# Appendix A：事实基线（v1–v4 考据，全部保留）

## A.1 版本与约束矩阵 ✅

| 项 | 值 |
|---|---|
| bevy_needle | 0.1.0（09-02）→ **0.2.0（09-08）**，MIT OR Apache-2.0，2498 行 |
| bevy 依赖 | `bevy_app/bevy_ecs/bevy_tasks =0.19.0` |
| bevy 0.19 MSRV | **1.95.0** |
| rig-core 0.42 MSRV | **1.98**（0.41 是 1.97） |
| bevy_needle 0.2.0 MSRV | **1.98** ✅ 已修 |
| bevy_rig | 0.1.0，bevy 0.18.1 + rig-core 0.33 → **版本不兼容，只能抄命名** |
| rig / rig-core | 0.41.0（07-28）、0.42.0（08-17），节奏 17–20 天 |

## A.2 bevy_needle 关键类型原文 ✅

```rust
pub trait NeedleBackend: Send + Sync + 'static {
    fn bind(&self, signature: u64, system: &str, tools_json: &str,
            tool_index: Option<&Path>) -> Result<(), NeedleError>;
    fn complete(&self, input: &str, max_new_tokens: u32, buffer: &mut [u8]) -> Result<..>;
    fn reset(&self);
    fn load_weights(&self, blob: &[u8]) -> Result<(), NeedleError> { .. }  // provided
    fn buffer_size(&self) -> usize { .. }                                 // provided
}
// DEFAULT_BUFFER_SIZE = 64 KiB；required 只有 bind / complete / reset

pub enum NeedleRunError {   // 运行层
    NoSnapshot(String), EngineUnavailable(String), ChannelClosed,
    BelowConfidence { confidence: f64, threshold: f32 }, Engine(String),
}
pub enum NeedleError {      // 引擎/信封层
    EngineNotFound, LibraryLoad, MissingSymbol, InitFailed(i32),
    CompleteFailed(i32), LoadWeightsFailed(i32), InteriorNul,
    BufferNotTerminated, BadUtf8(Utf8Error), BadEnvelope { source, raw },
}
// ⚠️ BadEnvelope 在 NeedleError，不在 NeedleRunError —— 两个都要消费

pub enum NeedleEngineStatusKind { Injected, Ready, Unavailable }
pub struct ToolSpec { name: String, description: String, parameters: Value }
pub struct ToolCall { run: Entity, tool: Entity, name: String,
                      call_id: String,   // 稳定唯一 ID（run/tool/nonce）
                      args: Value }
pub enum ToolDispatchPolicy { RegistryHandler, External }
pub enum ChatMessageRole { System, User, Assistant, Tool }
```

- `tool` 有**两个**注册表：`ToolRegistry`（schema）+ `ToolHandlers`（handler，按名）
- `ToolHandlerFn` 是不需要 `World` 的同步纯函数
- `needle_runtime`：「所有 run 的 turn 请求都在同一条工作线程上排队（引擎是进程级单会话）」
- `collect_transcript` 按显式 `ChatMessageSeq` 排序

## A.3 rig 已发布版（0.41 / 0.42）✅

```rust
AgentRun::{ new, with_history, max_turns, next_step, model_response, tool_results }
AgentRunStep::{
    CallModel { prompt: Message, history: Vec<Message>, turn: usize },
    CallTools { calls: Vec<PendingToolCall> },
    Done(PromptResponse),
}
// AgentRun: Send + Sync + Unpin + UnwindSafe，且 Serialize + Deserialize
// 官方定位：「Use AgentRun only when you need to hand-drive model and tool IO」

#[non_exhaustive]
pub struct PendingToolCall {
    pub tool_call: ToolCall,
    pub preresolved_result: Option<..>,  // 非空时必须直接回灌，不许执行工具
    pub internal_call_id: Option<..>,    // 流式轮次相关 ID，须保持一致
}

AgentBuilder::{ tool, dynamic_tool, dynamic_tools(Vec<DynamicTool>), add_hook, tool_server_handle }
// ⚠️ 无 ToolDyn —— 那是 0.4–0.20 时代 API，0.41 已删

DynamicTool::new(name, description, parameters: Value, callback: F)
// F: for<'a> Fn(&'a mut ToolContext, Value) -> WasmBoxedFuture<'a, Result<..>>  ← async
PortableDynamicTool::new(..)   // context-free，PR-B 应优先用它

impl<M: ConversationMemory + ?Sized> ConversationMemory for Arc<M>   // blanket impl 成立
impl<M: ConversationMemory + ?Sized> ConversationMemory for Box<M>
```

**rig-core 0.42.0 已发布 Cargo.toml（原文）**：
```toml
[features]
default = ["reqwest", "derive", "rustls"]
[dependencies.reqwest] version = "0.13" ...
[dependencies.tokio]   version = "1"  features = ["rt", "sync"]   # 无 optional
```
→ reqwest 可由 `default-features = false` 去掉，**tokio 去不掉**。

**rig-candle 限制（原文）**：
```rust
pub enum ModelArchitecture { Llama, Qwen3 }     // Llama 含兼容 SmolLM2
pub enum ConversationProtocol { Llama3, SmolLm2, Qwen3 }
```
- `spawn_blocking` · `max_concurrent_requests` 默认 1 · **stream channel 容量 8**
- 取消协作式 · 「A load that has entered the blocking pool runs to completion even if its awaiting future is dropped」
- CPU only（无 CUDA/Metal、无 batching、无 multimodal、无分片）

## A.4 Spark-X2.5 调查 ✅

```
model_type = spark2_5 ; architecture = Spark2_5ForCausalLM
max_position_embeddings = 1,048,576 ; sliding_window = 512 ; layers = 36
混合注意力（1 全注意力 + 3 滑窗）· Apache 2.0
```
**上游生态全部需打补丁**：llama.cpp → `XHToken/llama.cpp` fork；vLLM → `XHToken/Spark-plugin`；SGLang → pinned nightly + `--tool-call-parser spark25`；MLX → `XHToken/Spark-MLX-LLM`。

**结论：连 llama.cpp 都跑不了原版，candle 更不可能。** 正确解法是 §4 的 `LazyModel` 档位存**构造函数**（`Box<dyn Fn() -> Box<dyn CompletionModel>>`），用户塞 Ollama / vLLM 端点即可，无需任何上游贡献。

---

# Appendix B：PR-A 历史与验收

## B.1 四项验收：全部通过 ✅

| # | 条目 | 证据 |
|---|---|---|
| 1 | MSRV → 1.98 | `rust-version = "1.98"`；crates.io 显示 v1.98.0 |
| 2 | `policy.rs` 无 cfg | 模块文档明写「本模块无 `#[cfg]`、无 rig 依赖」 |
| 3 | 补 `RunEscalation` 写入点 | 三个新函数 + 状态机文档 |
| 4 | `Escalated` 变体 + `Failed` 净化 | 7 变体，`Failed` 文档去掉「置信度门控」 |

**附加达成（计划外但正确）**：`finalize_escalations` 兜底了「无 driver 时 `Escalating` 悬挂」，并显式留出 PR-B 接管点。

**依赖红线核验**：0.2.0 依赖仅 `bevy_*` + `serde` + optional `libloading`，无 rig / tokio ✅

## B.2 发现的问题：3 项待修

| # | 问题 | 优先级 |
|---|---|---|
| **P1** 🔴 | `Escalated` 是真实 break。下游 `match { Completed \| Failed }` 会静默漏分支，需补迁移提示 | 最高 |
| **P2** 🟡 | `EscalationTarget::Needles` 复数，与 `rank()` 文档 `0=Needle` 不一致 | 趁早 |
| **P3** 🟡 | prelude 未导出 policy 面，宿主需写全路径 | 随 PR-B |

---

# Appendix C：上游 bevy-prep 证据

## C.1 三条 PR 核验 ✅

| PR | 标题 | 状态 | base branch | 日期 |
|---|---|---|---|---|
| #2397 | `rig-reqwest` — cut the bundled transport；rig-core 无 reqwest/tokio | Merged | **`main`** | 08-21 |
| #2403 | `rig-run` — extract the sans-IO run protocol out of rig-agent | Merged | **`main`** | 08-22 |
| #2446 | `Pre-rig-bevy: the bus as a host can drive it` | Merged | **`feat/effect-bus`** ⚠️ | 09-03 |

**#2403 原文**：「a new crate rig-run (rig::run) that depends on rig-core only — no async runtime, no hooks, no tool registry — so rig-agent's futures driver and **a coming Bevy-app plugin** step the same AgentRun」
**#2446 原文**：「`Pending::poll_outcome` / `EffectStream::poll_item`: one poll under a no-op waker, no executor」

## C.2 rig-run 依赖闭包（Cargo.lock 实证）✅

```toml
[[package]] name = "rig-run" version = "0.42.0"
dependencies = ["http 1.4.2", "rig-core", "serde", "serde_json", "thiserror 2.0.18", "tracing"]
```
**无 tokio / reqwest / futures。** 且上游配了 dependency/no-async CI guard。
**未发布**：`version = "0.42.0"` 而 rig 0.42.0 已于 08-17 发布、#2403 于 08-22 才合入 → 不可能包含其中。

## C.3 战略推断：上游正在自建 rig-bevy

1. #2403 动机提到「a coming Bevy-app plugin」
2. #2446 标题 `**Pre**-rig-bevy` —— 本体还没开始
3. 这条线有正式命名 **bevy-prep**，节奏密集

**结论：押协议层（rig-run），不押驱动层（rig-bus / 自建 runtime）。** 上游的 Bevy 插件大概率不知道 needle2 存在，不会实现「置信度门控 → 升级」这条专属语义。

## C.4 未解偏差

⚠️ **#2397 后 main 的 rig-core 是否仍含 tokio**：抓取显示 `tokio = { workspace = true, features = ["full"] }`。五步推理指向 **dev-dependency**（workspace 是裸 `tokio = "1"`；runtime 依赖不会开 `["full"]`；抓取工具折叠了段落；报告特意用 `-e normal` 排除 dev）。**置信约 80%，须用命令证伪**：

```bash
cargo tree -e normal --git https://github.com/0xPlaygrounds/rig \
  -p rig-core --no-default-features | grep -E "tokio|reqwest"
```
- 无输出 → #2397 生效，`rig-local` 可做到完全零 tokio
- 有 tokio → #2397 被回退，pin 到 #2397 merge commit 而非 main

---

# Appendix D：反面教训（20 条）

## D.1 API 层面

1. `BadEnvelope` 在 **`NeedleError`** 不在 `NeedleRunError`（只消费一个会漏 FFI 解析失败）
2. `ToolDyn` **在 0.41 已删除**，用 `DynamicTool` / `PortableDynamicTool`
3. `DynamicTool` 回调**是 async**，同步 `ToolHandlerFn` 应包成立即就绪 future，不需 `spawn_blocking`
4. `AgentBuilder` 只有单一 `add_hook(H: AgentHook)`，无事件特定方法
5. `dynamic_tools(Vec<DynamicTool>)` 存在，可批量注入（typed `tools(Vec)` 才没了）
6. `buffer_size` / `load_weights` 是 **provided**，不是 required
7. `RunId` 现在是 `NonZeroU64`（进程级 `AtomicU64`），`as_str()` 已移除

## D.2 架构判断层面

8. 冷启动路由不能插在 `RunPreparation` 之前 —— 那个阶段才创建 run 实体
9. 帧预算不只是「别误判卡死」—— candle stream channel **容量 8** 会反压，drain 预算须 >8
10. **`NoSnapshot` 不该升级** —— 是开发者错误，升级救不了
11. rig-candle 限制是**架构枚举**不是模型白名单；Spark-X2.5 的墙在它是非 stock 架构
12. `ConversationMemory` 只管 rig 段内部，needle 段须先快照 → 两段式
13. 「cargo tree 不出现 tokio」在 rig-core 0.42.0 上**不可达成**
14. #2446 合并进 `feat/effect-bus`，**不在 main 上**
15. `default-members` 没有 rig-run 是**弱信号**（是 `members` 的子集），真正证据是版本号

## D.3 元层面

16. `bevy_rig` 锁 bevy 0.18.1 + rig-core 0.33，与 bevy 0.19 不兼容，**只能抄命名不能抄代码**
17. MSRV 必须先查再动手 —— 查完发现已是现存 bug
18. 文档摘要会吞掉泛型（`Vec` 而非 `Vec<PendingToolCall>`），要编译的类型必须读源码
19. PR 合并日期不等于可用 —— 必须同时看 state 与 **base branch**
20. Cargo.lock 的 diff 比 Cargo.toml 更有说服力（前者是已解析依赖图）
