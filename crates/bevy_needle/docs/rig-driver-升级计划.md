# bevy_needle PR-B 执行规格：Driver 契约与 rig 适配

> 版本：**v21**（2026-09-23）· 面向 Claude Code 执行
>
> **✅ v14.5：停止支持 needle2，Gen3 是唯一代际**（维护者决策）。引擎产物已入库
> （`third_party/needle/3.0.1/{libneedle3.so, needle3.cact, needle.h}`，头文件 ABI
> 与 ffi 绑定逐一核对：`needle_last_error` 存在、embed 两段式 3072 维实证）。
> **真机全链冒烟 PASS 并转正为测试**（`needle3_real_smoke`：open → load base →
> init → complete → embed → reset；信封与 v2 完全兼容 + 新增
> prefill_tps/decode_tps/peak_ram_mb 性能字段）。`EngineGeneration` 收敛为单一
> Gen3；`EngineConfig` 去 `with_generation`、`with_base_weights` 为唯一权重入口。
>
> **✅ v14.4：needle3 代际适配**（上游 needle v3.0.x 已发布，PyPI `cactus-needle==3.0.1`）。
> 上游从 v3 起引入 generation：库文件名带代际后缀（`libneedle3.so`）、缓存按代际分轨
> （`~/.cache/cactus-needle/v3/`）、**基础权重不再打包进库**（gen≥3 首次 bind 前必须
> `needle_load(needle3.cact)`，顺序 `_bind → _load_base → needle_init`——已按上游源码
> 核实）、新增 `needle_embed` 符号（两段式：null/0 问维度，再取 f32 向量）、`.cact`
> 带 generation tag（0x05E12A83=2 / 0x05E12A84=3，跨代喂权重必炸）。
> 适配：`EngineGeneration`（默认 Gen2 不变）+ `EngineConfig::{with_generation,
> with_base_weights}` + `discover_library_for` + `DlopenBackend::{open_for, embed,
> supports_embed}`；信封结构（type/function_calls/confidence）与 v2 兼容，
> `confidence: null` 由既有 `Option<f64>` 承接。测试 +5（常量/路径/分轨/响亮失败/默认不变）。
>
> **🟡 待定提案（§19）**：「大幅精简 → 删除 `RigDriver<M>` → 直连 rig-ecs」。
> 裁决：**方向可做 POC，主线架构不动**。三条反对（前提未成立 / 掏空 Policy 会丢 API key 红线 /
> 工具桥只是换位置不消失）+ 两条采纳（**即日冻结 `DriverRegistry` 能力面** / POC 改名并补 4 条验收）。
>
> **🔴 上游已变（09-01 / 09-15）**：
> 1. **`rig-run` 已被删除**（#2432），`AgentRun` 迁到 `rig_agent::run` —— 原「等 rig-run 发布」策略作废
> 2. **rig-agent 关掉 default features 后零运行时**（有 CI guard）→ 这才是解 crates.io pin 的正路
> 3. **rig-ecs 已合入 main**（#2529，09-15）→ **不迁移**，理由见 §18.4（它不 step AgentRun，且要 bevy 0.19.1）
>
> **🎉 v14.3（`e3728398`）审查 PASS**：组合注册事务、`[0,0]` 反例、判定核心同源、T3-A 两档真执行、I28 取消快照 全部通过。
> **PR-B 主体完成**，剩 P2 收尾与 crates.io 发布（等 rig 0.43 解 git pin）。阶段性总结见 **§17**。
>
> **📌 架构定位（维护者裁决，2026-09-22）**：
> **`bevy_needle` = Needle 的 Bevy integration + resolution/escalation layer；
> 通用 Agent runtime 可由 Rig / rig-ecs 承载。**
> - `RunResolution`（置信度/tier/fallback ceiling/epoch/升级 handoff）是 bevy_needle
>   相对 `bevy_rig` → `rig-ecs` 谱系的**真正原创边界**——通用 Agent runtime 由上游正规化，
>   Needle 专属决策语义留在这里；
> - `rig-ecs` 是 **Driver/handoff 的执行目标**，不是「另一个 Driver 实现」——
>   未来关系是 `Resolution → handoff → rig-ecs runtime`，且必须**同一个 Bevy World**
>   （rig-ecs 自述 "a Bevy app now"；禁止嵌套第二个 World）；
> - `EscalationPolicy`（含 tier 与 capability ceiling）**不在任何删除树里**——
>   `LocalModelOnly` 对玩家机器分发是隐私红线，不是路由便利（§19.2）；
> - `DriverRegistry` 能力面**即日冻结**（§19.4）：修 bug/补测试可以，remote/human
>   driver / provider discovery 全部推到 handoff POC 结论之后；
> - 不写 `RigEcsDriver`（POC 改名 **Needle → Rig-ECS Handoff POC**，§19.5）。
>
> **（历史横幅存档）**「DriverId 覆盖 bug 为当前第一优先级」——已在 v14.1/v14.2 修复并经
> v14.3 审查 PASS（§16 / §17.4），横幅仅作时间线留档。
>
> **v13 → v14 的两条硬规则**
> 1. **`register()` 与 `bind_tier()` 必须彻底拆开** —— 否则「重复 ID 报错」会顺带杀掉
>    「同一模型绑定多个 tier」这个合法能力（§16.5a）
> 2. **rig 身份集合按模型类型隔离** —— `RigDriverIds<M>` per-`M`，不是全局 `HashSet`（I31）
> 另：清理了 §2 / §4.1 / §4.3 中残留的 `Local(LazyModel)` 过时形状。
> 前序版本 v1–v9 的事实考据**全部保留**，已移至 Appendix，未删除。
>
> **当前状态：PR-B 全部主体完成。CompletionModel 已接线，AI 真正跑通。**
> 测试：rig 46 / escalate 32 / default 28，全绿。剩 P2 收尾与 crates.io 发布（等 rig 0.43 解 git pin）。
>
> **v9 → v10 的变化**
> - **RPITIT 发现**：`CompletionModel` 因返回位置 impl trait **不可 `dyn`**，`Box<dyn CompletionModel>` 根本不成立 → `LazyModel` 整条设计作废，改为 `RigDriver<M: CompletionModel>` 泛型持有 `Arc<M>`（§13.1）
> - **I18 审计结论**：本地代码**本就严格**，无 `new_or_mint` 违规 —— 此前外部审查的指控不成立，P0 关闭
> - 新增 **I27 回执纪律**：worker 对每件事必须给交代，禁止静默丢弃（这是调试中抓到的真 bug）
> - 新增 **I26 状态封装**：`RigDriverState` 内部状态只读，模型回合只能经 `RigModelInbox` 注入
> - 记录 bevy 0.19 一个坑：手动 `impl Resource` 会导致 `insert_resource` 后取不到，必须 `#[derive(Resource)]`
> - I25 已落地：Driver 声明 capability，注册期校验
>
> **v7 → v8 的变化**
> - **四个 P1 全部裁决完毕**（§10），不再是开放问题：`DriverRegistry` 两层结构、`EscalationPolicy` = Resource、`DriverAttempt` 不做 Component、`LazyModel` 限制在 rig adapter
> - 新增 **I23 / I24** 两条不变量：**主路径与升级路径不得合并**；**`LazyModel` 不得渗透进 `Driver` trait**
> - 新增 §4.7「DriverRegistry 两层结构」—— 解决「按 DriverId 注册」与「tier 下标解析」的冲突
> - crates.io 发布阻塞（git pin）**按维护者决策降级**，记入 §9.3，不再阻塞实现
> - 记录一处**未能核实的基线分歧**（v8 §12.1）：CompletionModel 是否已接线
>
> **v6 → v7 的变化（保留备查）**
> - 删除 `Driver::poll()`（I16），改 channel 回灌
> - `EscalationTarget` 去掉 `LazyModel` 载函（§2.3，两项 breaking 待维护者决策）
> - tier 是下标不是 rank（I10 反例）
> - `EscalationState` 补齐 `epoch / driver_id / deadline / reason`
>
> **v4 → v5 的结构变化**
> - 证据与契约分层：§0–§10 是可执行契约，考据移入 Appendix A/B/C
> - **`Driver` 从隐含概念升为第一等架构对象**（新增 §4）
> - 新增 §1「不可协商不变量」、**§11「禁止实现模式」**
> - 新增 §7 错误/取消/超时行为表、§8 状态机测试矩阵
> - 开放问题按 P0–P3 分级
> - 采纳两条语义澄清：**Driver ≠ Tool ≠ NeedleBackend ≠ Agent**；**Bevy Run ⊃ AgentRun**
> - 采纳 `EscalationState` 与 `RunStatus` 分离、**限制 `finalize_escalations` 为安全阀**
> - 采纳 feature 拆分 `escalate`（能力）/ `rig`（实现）
> - 采纳「tokio 问题降级为依赖维护任务，不进架构」

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

## §1 不可协商不变量（24 条）

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
| **I23** | **主路径与升级路径不得合并**。`NeedleBackend` / `needle_complete()` 归 `RunExecution`；`Driver` / `CompletionModel` 归 escalation。**不得构造一个同时包住两者的「统一 Model 抽象」** |
| **I24** | **`CompletionModel` 不得渗透进 `Driver` trait**。`Driver` 只认 `DriverId` / `run` / `epoch` / input / tool context / result；模型是什么属于 `RigDriver<M>` 的实现细节。注意：`CompletionModel` 是 RPITIT trait，**不可 `dyn`**，所以连"擦除后塞进 trait"都不可能 —— I24 由编译器强制 |
| **I25** | **Driver 必须声明 capability**（`Needle` / `Local` / `Remote`）。注册期校验 tier binding 的 capability 与 `EscalationTarget` 匹配；不匹配 → 该 tier 不可用 → 走 `Escalated`（没试）不是 `Failed`。**capability 只用于校验，绝不用于推导 tier**（I10） |
| **I26** | **`RigDriverState` 内部状态只读**。`awaiting` / `tool_turn` 不得外部可变访问；模型回合**只能**经 `RigModelInbox` 注入，禁止从别处直接塞 `ModelTurn` |
| **I27** | **回执纪律：worker 必须对每个请求给出交代**。completion 失败 → 标记失败；通道断开 → 传输错误；收到无主响应 → `DriverUnavailable`。**禁止静默丢弃** —— 静默会让 run 永久卡在等待态 |
| **I28** | **Driver resolution 是 attempt-stable**：一次 attempt 成功 resolve 的 Driver 实例在其生命周期内固定，registry 后续 mutation 不得改变该 attempt 的执行者；事件能否提交仍由 `(run, epoch)` 决定。**不做「运行期冻结整个 registry」** —— 那会堵死 P3 动态 provider |
| **I29** | **身份归 Driver 实例所有，Registry 只拒绝重复**。`Driver::id()` 返回自身字段；`register()` 遇重复 `DriverId` 报 `DuplicateDriverId` 而非 `HashMap::insert` 覆盖；`bind_tier()` 遇重复报 `DuplicateTierBinding`，显式覆盖走 `rebind_tier()`；两个操作都必须是**事务**的（失败不得留下半注册状态） |
| **I30** | **禁止任何代码依赖固定 driver 身份常量做行为判断**。`RigDriver::id()`、`capability()` 都必须是实例字段。**已知三处违规**：`id()` 返回常量 `"rig"`；`capability()` 硬编码 `Local`；`ensure_states()` 用 `esc.driver_id == RIG_DRIVER_ID` 认领 run |
| **I31** | **rig 侧身份集合必须按模型类型隔离**：`RigDriverIds<M>`（per-`M` 的 `HashSet<DriverId>`），**不是**全局 `HashSet<DriverId>`。`ensure_states::<M>()` 只认领 `esc.driver_id ∈ RigDriverIds<M>` 的 run |
| **I32** | **三层一一对应关系**：一个 `M` 对应**一个** `RigModelInbox<M>` + **一个** `rig_step_system::<M>`；一个 `DriverId` 对应**一个** Driver 实例；一个 Driver 实例**可绑多个 tier**。多个 `DriverId` 可共享同一个 inbox/system |

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
pub enum EscalationTarget { Needle, Local, Remote }  // ✅ 现行 master 形状，无载函
impl EscalationTarget { pub fn rank(&self) -> u8 }   // 0 / 1 / 2，仅用于 capability 上限判断
pub enum OnlineFallback { Never, LocalModelOnly, Cloud }
// LazyModel 已从 policy 面移除（§2.3 已执行）；CompletionModel 是 RPITIT 不可 dyn，
// 模型改由 RigDriver<M> 泛型持有 Arc<M>（§13.1）

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

### 2.3 PR-B 内一次性完成的两项 breaking 变更（✅ 已在 master 执行）

> **状态更新（v13）**：本项**已完成**，不再是待办。实测 `policy.rs` 源码：
> ```rust
> pub enum EscalationTarget { Needle, Local, Remote }   // 无载函
> // 注释原文：「PR-B breaking（规格 §2.3）：不再携带 LazyModel 载函——
> //            模型构造能力属于 DriverRegistry（escalate 层），不变量 I7」
> ```
> `LazyModel` 在 `policy.rs` 中已不存在。以下保留的是当初的论证过程，作为决策留档。

**推翻 I15 的字面约束一次，理由是修形状而非另起一套。**

`EscalationTarget` 0.2.0 发布时的形状 `Needles / Local(LazyModel) / Remote(LazyModel)` 有两个缺陷：

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
EscalationTarget  描述「目标能力类别」              ← 0.2.0 已有（PR-B 去掉载函，§2.3 ✅ 已执行）
DriverRegistry    负责「tier → 哪个 Driver」        ← PR-B 新增，**只做身份/路由，不持模型**（I24）
Driver            负责「怎么执行」                   ← PR-B 新增
RigDriver<M>      持有 Arc<M> 与模型生命周期         ← 模型归 driver 实例，不归 core registry
```

> ⚠️ 修正（v14）：早期版本写过「DriverRegistry 持有 LazyModel」，与 I24 自相矛盾。
> **现行裁决**：core registry 只持 `Arc<dyn Driver>`；模型由 `RigDriver<M>` 自身持有。

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

DriverRegistry (Resource，只做身份/路由)
  ├─ drivers:   HashMap<DriverId, Arc<dyn Driver>>
  └─ tier_map:  HashMap<u32, DriverId>          ← 实测为 HashMap，非 Vec（§4.7）

RigDriver<M> (Driver 实例，持有模型)
  └─ Arc<M> + jobs sender → RigModelInbox<M>
```

**模型生命周期决策：归 Driver 实例，不归 core registry**

- Run 生命周期是帧级的、可取消的；模型初始化是秒级的、**不可取消**的（candle load 进 blocking pool 后 drop future 不停）。耦合会导致 run 取消时模型反复重建。
- `CompletionModel` 是 RPITIT 不可 `dyn`（§13.1），所以模型**只能**以 `Arc<M>` 由 `RigDriver<M>` 持有。这使 I24 由编译器强制。
- **宿主构造模型的位置是阻塞点** —— 必须在启动期完成，不要在 gameplay 首次升级时构造。

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

### 4.7 DriverRegistry 是两层结构（P1-6 裁决）

**问题**：有建议提出 `DriverRegistry { drivers: HashMap<DriverId, Arc<dyn Driver>> }`。这个形态**对，但不完整** —— 它只解决「id → 实例」，没解决「tier → 哪个 id」。

这正是 I10 那个反例的**另一件外衣**：

```
policy.tiers = [Local(Candle), Local(GPTQ), Remote]
tier:               0              1          2
DriverId:      "rig-local-a"  "rig-local-b"  "rig-remote"
```

若 Registry 只有 `DriverId → Driver`，`EscalationTarget::Local` 无法区分 tier 0 与 tier 1 —— **第二个 Local 档永远选不中**，和当初 `max(rank())` 是同一个 bug。

**裁决：两层，各管一件事**

```rust
#[derive(Resource, Default)]
pub struct DriverRegistry {
    /// 第一层：id → 实例。driver 是「东西」，按 id 查找。
    drivers: HashMap<DriverId, Arc<dyn Driver>>,
    /// 第二层：tier → id。与 EscalationPolicy.tiers 下标平行。
    /// 这是 tier 语义（I10）与 driver 实例之间的唯一桥梁。
    tier_bindings: Vec<DriverId>,
}

impl DriverRegistry {
    pub fn register(&mut self, id: DriverId, driver: impl Driver + 'static);
    pub fn bind_tier(&mut self, tier: u32, id: DriverId);
    pub fn driver_for_tier(&self, tier: u32) -> Option<&Arc<dyn Driver>>;
    pub fn get(&self, id: &DriverId) -> Option<&Arc<dyn Driver>>;
}
```

**为什么不让 `EscalationPolicy.tiers` 直接存 `DriverId`**：那会让 policy 知道 driver 身份，虽然 `DriverId` 是 opaque 字符串（不违反 I6），但会让 tier 的 capability 语义（`Needle` / `Local` / `Remote`，供 `OnlineFallback` 门控用）与 driver 身份耦合。两者分开，`OnlineFallback::LocalModelOnly` 仍能按 capability 拒绝 Remote，而不必认识任何 id。

**注册入口（Bevy 惯用 builder，落在 plugin 上）**

```rust
BevyNeedlePlugin::default()
    .driver("rig-local-a",  RigDriver::new(candle_a))   // register
    .driver("rig-local-b",  RigDriver::new(candle_b))
    .tier(0, "rig-local-a")                             // bind_tier
    .tier(1, "rig-local-b")
    .tier(2, "rig-remote")
```

⚠️ **`tier_bindings` 长度必须 ≥ `policy.tiers` 长度**，否则该 tier 无可用 driver → 按 §6 终态表走 `Escalated`（没试），不是 `Failed`。这条要进启动期断言。

### 4.8 tier 指向 `Needle` 时的陷阱（I23 相关）

`EscalationTarget::Needle` 是合法目标（为未来 Rig→Needle 回退预留，见 P2-5）。但**如果某个 tier 的 target 是 Needle，Driver 必须复用现有 `needle_runtime`，绝不能新建一条引擎通道**。

理由：`needle_complete` 是**进程级单会话**（所有 run 的 turn 请求在同一条工作线程排队）。新建第二条通道会破坏串行模型，且违反 I23。

建议：**第一版所有 tier 都绑定到非 Needle 的 driver**，`Needle` 变体仅作占位。等 P2-5 真正实现回退时，再让 `NeedleDriver` 薄封装现有 runtime。

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

### 9.3 crates.io 发布阻塞（已降级，不阻塞实现）

README 原文：

> ⚠️ 上游 rig-run 未发布，pin 到 #2403 merge commit；**crates.io 拒绝带 git 依赖的发布，发版前需临时摘除 pin**

**性质**：crates.io 不允许待发布包含任何非 registry 依赖，**包括 optional 的 git 依赖**。所以只要 `rig` 留在 Cargo.toml，整个 crate 发不出去 —— 连 `escalate` 一起被拖住。

**维护者决策（2026-09-10）：不着急发布，等 rig 更新。** 故此项**不再是阻塞项**，但发布前必须处理。三条出路备查：

| 方案 | 做法 | 代价 |
|---|---|---|
| **A. 等 rig 0.43** ✅ 当前选择 | 0.42 发布于 08-17，节奏 17–20 天 | 可能被 bevy-prep 拖长 |
| **B. 拆 companion crate** | `bevy_needle_rig` 承载 RigDriver + git pin，主 crate 保持干净 | 需把 `Driver` trait 转 public |
| **C. 发版前脚本摘 pin** | 发布流程里临时移除 | 每次发版手工操作，易错 |

⚠️ 注意 B 的可行性已变化：当年否掉「外层 crate」方案的理由是拿不到内部转录与注册表，但**工具桥已确认走 ECS pipeline，`ToolRegistry` / `collect_transcript` 都是 pub**，在这条窄接口下 B 是成立的。若 A 迟迟不来，优先 B 而非 C。

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

**P1 —— 已裁决（v8）**

| # | 裁决 |
|---|---|
| P1-1 | ~~超时时间源~~ → **进 `EscalationState.deadline`**（§4.4），不扩展 `RunStatus` 变体 |
| P1-2 | 取消语义：协作式。candle load 不可中止，靠 epoch 失效兜底（§7） |
| **P1-3** | **`DriverAttempt` 不做 Component**。当前 attempt 信息已由 `EscalationState` 承载（tier/attempt/epoch/driver_id）；历史 attempt 用事件流出。**不要为 ECS 化而 ECS 化** —— Component 表示需要被查询、被生命周期管理或跨系统观察的数据，纯 coordinator bookkeeping 不该强行 Component 化 |
| **P1-4** | **`EscalationPolicy` = Resource**。决定性理由：**tier 下标只有在共享同一份 `tiers` 与 `tier_bindings` 时才有意义**。若 policy 是 per-agent Component，各 agent 的 `tiers` 长度可能不同，tier 2 在 A 指 Local、在 B 指 Remote，tier→driver 映射直接崩。per-agent 差异已由 `NeedleAgentSpec::confidence_threshold` 覆盖 |
| **P1-5** | **`LazyModel` 限制在 rig adapter**，不进 `Driver` trait（I24）。但**必须先读本地源码确认实际形态** —— docs.rs 折叠成 `Arc<Box<..> + Send + Sync>`。若仍为 `Fn() -> Box<dyn CompletionModel>`，建议改为可失败：`Fn() -> Result<Box<dyn CompletionModel>, ModelError>`，因为模型加载会失败且 `Fn` 必须可重试 |
| **P1-6** | **`DriverRegistry` 两层结构**：`HashMap<DriverId, Arc<dyn Driver>>` + `Vec<DriverId>`（tier→id，与 `policy.tiers` 平行）。详见 §4.7 —— 只做第一层无法区分同 capability 的不同 tier |

**P1 剩余待确认（实现中可定）**

| # | 问题 |
|---|---|
| P1-7 | `LazyModel` 真实签名（读本地源码，见 P1-5） |
| P1-8 | `tier_bindings.len() >= policy.tiers.len()` 的启动期断言放在哪（`Plugin::finish` vs 首个 run 提交时） |

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

── v8 新增 ──────────────────────────────────────────────
❌ 把 NeedleBackend 与 Driver 合成一个「统一 Model 抽象」（违反 I23）
❌ 让 RigDriver 复用 needle 工作线程执行 model call    （违反 I21/I23）
❌ tier 指向 Needle 时新建第二条引擎通道               （违反 §4.8，破坏进程级单会话）
❌ LazyModel / CompletionModel 出现在 Driver trait 签名里（违反 I24）
❌ DriverRegistry 只做 DriverId→实例、没有 tier→id 绑定 （违反 §4.7，同 capability 多档会坍缩）
❌ EscalationPolicy 做成 per-agent Component           （违反 P1-4，tier 下标会失去意义）
❌ 为「ECS 化」把纯 bookkeeping 强行做成 Component      （违反 P1-3）
```

> **一条容易误伤的边界**：`poll` 不是一律禁止。rig #2446 的 `Pending::poll_outcome` 是**对 sans-I/O 状态机的 poll**，合法且好用；禁止的是**对 async future 的 poll 出现在 `Driver` 契约面**。前者归 transport 适配层，后者归 worker。

## §12 基线分歧（已关闭）

曾有两种说法冲突：外部审查称 `CompletionModel` 已接线，维护者清单标未完成。

**本地验证结论：未接线（清单正确）**，证据：
- `rig/driver.rs:37-45` `submit()` 只回 `AttemptHandle`，无模型调用
- `rig/transport.rs:10-12` 「CallModel 的真实 provider 执行留待下一阶段」
- `rig/agent.rs:415` 「生产 worker 在这里消费 (prompt, history, turn) 并回灌 RigModelTurn」
- `rig/agent.rs:109-112` `RigModelInbox` 设计上是**让外部注入**，不是自己调用

同时确认 **I13 成立**：`cargo tree -p bevy_needle --features rig -e normal | grep rig-agent` 无输出，依赖链为 `rig-core` + `rig-run`（同 git rev）+ `tokio`。

→ **v10 已接线完成**，本节仅作历史记录。

## §13 CompletionModel 接线（✅ v10 已完成）

### 13.1 RPITIT 发现 —— `LazyModel` 整条设计作废

**`CompletionModel` 的方法返回 `impl Future`（RPITIT），因此该 trait 不是 dyn-compatible，`Box<dyn CompletionModel>` 在 Rust 里根本无法构造。**

这直接废掉了 v1–v8 一直沿用的假设：

```rust
// ❌ 不可能成立
pub type LazyModel = Arc<dyn Fn() -> Box<dyn CompletionModel> + Send + Sync>;
```

**实际形态**：

```rust
pub struct RigDriver<M: CompletionModel> {
    model: Arc<M>,          // 具体类型，编译期确定
    // ..
}
impl<M: CompletionModel> Driver for RigDriver<M> { .. }

// 宿主注册
app.register_rig_driver(openai_model, &[0, 1]);   // tier 0/1 用这个模型
```

**为什么这反而更好**：
- I24 由**编译器**强制 —— 不是"不得渗透"，是"物理上无法渗透"
- 多模型不冲突：`RigDriver<Candle>` 与 `RigDriver<OpenAI>` 是两个具体类型，各自 `impl Driver`，可以同时作为 `Arc<dyn Driver>` 放进同一个 `DriverRegistry`
- 少了工厂函数与 `Result` 分支，代码更短

**连带作废的设计**：v8 §13 的「`Plugin::finish()` 启动期预热 + `Plugin::ready()` 门控」。
模型现在由**宿主在注册时构造**，不再是 lazy factory。这带来一个新责任：

> ⚠️ **宿主构造模型的位置现在是阻塞点**。若 `M` 是 candle 模型，加载权重是秒级阻塞。**宿主必须在启动期完成，不要放在 gameplay 中首次升级时**。建议在 `Plugin::finish()` 或更早构造，并考虑走 `bevy_tasks` 的 blocking 池。
> 后续可考虑 `RigDriver::from_factory(..)` 恢复惰性，但**必须**是泛型工厂（`FnOnce() -> M`）而非 `dyn` 工厂。

### 13.2 实际接线链路

```
ECS:  AgentRun::next_step() → CallModel { prompt, history, turn }
  ↓ DriverCommand::CallModel { run, epoch, request }
worker:  model.completion(req).await        ← 唯一 async 点
  ↓ ModelTurn
RigModelInbox                                ← 唯一注入点（I26）
  ↓
ECS:  AgentRun::model_response(turn) → 继续步进 → Done → Completed
```

### 13.3 接线中抓到的真 bug → I27 回执纪律

**现象**：worker 出错时静默丢弃 → run 永久卡在「等待模型回答」，不失败也不完成。

这正是 §6 终态表要防的"悬挂"，只是发生在 worker 内部而非 ECS 侧。**修复方式：worker 对每件事必须回执**：

| worker 遭遇 | 必须回执 |
|---|---|
| `completion` 失败 | 标记失败 |
| 通道断开 | 传输错误 |
| 收到无主响应（无匹配请求） | `DriverUnavailable` |

**禁止静默丢弃** —— 这是 I9「绝不静默悬挂」在 worker 侧的延伸，已升格为独立不变量 I27。

### 13.4 仍然有效的两个约束（做 P2 流式时别忘了）

- candle `max_concurrent_requests` 默认 1 → worker 侧信号量串行化（**只对 candle**，远端可给 4）
- candle stream channel **容量 8** → 每帧 drain 预算必须 >8，否则推理线程阻塞在 send 上，直接违反「不卡帧」

### 13.5 bevy 0.19 的一个坑（已修）

```rust
#[derive(Component)]
struct X { .. }
impl Resource for X { }     // ❌ 手动 impl：insert_resource 后 ECS 取不到
```
需改用 `#[derive(Resource)]`。已修，此处留档 —— 同类写法在别处出现时会复现同样的"资源莫名失踪"。

---

## §14 一句话总结

**0.2.0 已把「升级」钉成契约，PR-B 只补「谁来接管」。** 核心是让 `Driver` 成为第一等架构对象：Policy 纯决策、Driver 纯执行、`RunResolutionSystems` 只认 `DriverId`、工具永远走 ECS。这样 `bevy_needle` 的核心**永远不知道 rig 是什么**，rig 只是一个可选 driver，未来加 OpenAI / Candle / Human 都不用改核心。

**历史关键修正（v5–v8）**：
1. **`Driver` 无 `poll`** —— 异步推进归 worker，结果经 channel 回灌，ECS 只 drain。否则每个 Driver 都要重造一个简化版 executor
2. **`EscalationTarget` 去掉 `LazyModel`** —— Policy 才真正是纯决策（I5/I7）
3. **tier 是下标不是 rank** —— `[Local(Candle), Local(GPTQ), Remote]` 里两个 Local 的 rank 相同，用 rank 推 tier 会让第二个档永远选不中

**v10 最关键的一条**：**`CompletionModel` 是 RPITIT trait，不可 `dyn`** —— 这让 I24 从架构纪律变成**编译器保证**，同时让 `LazyModel` 整条设计作废。`RigDriver<M: CompletionModel>` + `Arc<M>` 是唯一正确形态。

**以及最重要的一句话（保持不动）**：**Bevy Run ⊃ AgentRun**。绝不让 agent runtime 反过来成为应用 runtime —— 这是 AI agent 项目最常见的错误，对 ECS 框架尤其致命。

---

## §15 v10 收尾清单

**已完成**：`EscalationPolicy` / `Driver` + `DriverRegistry` / `RigDriver` + tool_bridge / **CompletionModel 接线** / I25 / I26 / I27 / FakeModel e2e；**§2.3 两项 breaking 已在 master 执行**（`policy.rs` 实测无 `LazyModel`）。
测试：rig 46 ✅ · escalate 32 ✅ · default 28 ✅。

---

## §17 阶段性总结（v16 · 截至 `e3728398`）

### 17.1 交付时间线

| 日期 | 提交 | 内容 |
|---|---|---|
| 09-02 | `v0.1.0` | Needle 引擎进 Bevy ECS；2498 行、14MB 模型、no async runtime |
| 09-08 | `v0.2.0` | 升级契约：`Escalating` / `Escalated` 两个新状态，`Failed` 语义净化，MSRV 1.85→**1.98** |
| 09-09 | `feat(escalate)` PR-B | Driver 契约 + rig 适配；feature 三分 `dlopen`/`escalate`/`rig` |
| 09-09 | `fc0ec414` | **CompletionModel 接线** + I25/I26（RPITIT 定案、FakeModel e2e） |
| 09-09 | `fe38cc8d` v14.2 | 组合注册事务化 + I28 取消侧快照 + T3-A 两档真执行 |
| 09-09 | `e3728398` v14.3 | `DuplicateTierArgument` + 判定核心同源 + 审阅实证修正 |

**净成果**：从一个"低置信度直接进 Failed"的空路径，变成一条**可在三档之间 handoff、工具永远走 ECS、主线程永不阻塞**的完整升级链路，且默认构建仍然零 rig、零 tokio。

### 17.2 最终架构（一张图）

```
RunAgent → RunPreparation → RunExecution(needle 工作线程)
                                   ↓
                            RunResolution
                                   ↓
                          Escalating { tier }
                                   ↓
              EscalationPolicy → DriverCoordinator → DriverRegistry
                                   ↓                  (tier → DriverId → Arc<dyn Driver>)
                          ┌────────┴────────┐
                     RigDriver<M>       FutureDriver
                          ↓
                     AgentRun（ECS 侧手动步进）
                          ↓
              CallModel ──→ worker（唯一 async 点）
              CallTools ──→ ECS ToolInvocation
                          ↓
                     RigModelInbox → model_response → Done
```

三条边界始终没有被跨过：
1. **主线程永不 `block_on`**
2. **工具执行永远在 ECS**（`PendingToolCall → ToolInvocation → dispatch → 按原序回灌`）
3. **Bevy Run ⊃ AgentRun**（`AgentRun` 只是一次 attempt 的内部状态）

### 17.3 十个决定性决策（及为什么）

| # | 决策 | 反面 |
|---|---|---|
| 1 | 落点在 **Run 状态机层**，不在 `NeedleBackend` | A 层 `complete()` 是同步固定缓冲，塞 `block_on` 会卡死所有本地 run 并丢弃 rig 全能力 |
| 2 | `Driver` 升为第一等架构对象 | 否则 rig 会退化成"注册一个 tool"，架构跑偏 |
| 3 | **Bevy Run ⊃ AgentRun** | 反过来就是"agent runtime 成为应用 runtime"，AI agent 项目最常见的错误 |
| 4 | `Driver` **无 `poll`** | 有 poll 就要自存 Future/管 waker/感知 runtime = 重造简化版 executor |
| 5 | **tier 是下标不是 rank** | `[Local(Candle), Local(GPTQ), Remote]` 两个 Local 的 rank 相同，用 rank 推 tier 会让第二档永远选不中 |
| 6 | `EscalationTarget` **去掉 `LazyModel` 载函** | 否则 Policy → Target → Model，Policy 最终又知道执行 |
| 7 | feature 拆 `escalate`（能力）/ `rig`（实现） | 合一则 `escalate` 直接拽进 rig+tokio，MockDriver 无法脱离 rig 验证契约 |
| 8 | **RPITIT**：`CompletionModel` 不可 `dyn` → `RigDriver<M>` | 让 I24 从纪律变成**编译器保证** |
| 9 | **I23 主路径与升级路径不合并** | 合并会让 needle 的同步单会话引擎与 rig 的 async agent 重新揉成一个"所有 provider 都长一样"的抽象 |
| 10 | **epoch + attempt-stable 快照** | 没有 epoch 就无法区分"取消后的迟到响应"，旧响应会复活 Run |

### 17.4 审查循环抓到的真 bug（这份计划最值钱的产出）

| Bug | 性质 |
|---|---|
| `RigDriver::id()` 固定 `"rig"` → 多模型互相覆盖 | **三处固定身份**（`id`/`capability`/`ensure_states`）必须原子改，只改一处得"假绿" |
| `capability()` 硬编码 `Local` | 后果最隐蔽：远端模型**永远**无法通过 I25 校验注册 |
| `[0,0]` 输入自重复 → 半注册 | precheck 只查 registry，不查入参；且 precheck 与执行路径语义不一致 |
| `RigModelInbox<M>` 无条件 `insert_resource` | 同 M 多 ID 时覆盖；修好 ID 后反而更容易踩到 |
| worker 出错静默丢弃 → run 永久悬挂 | 升格为 **I27 回执纪律** |
| bevy 0.19 手动 `impl Resource` 取不到资源 | 必须用 `#[derive(Resource)]` |
| MSRV 声明 1.85 而 bevy 0.19 要 1.95 | **发布前就存在的 bug**，独立于本次 PR |

### 17.5 三条元经验

1. **外部审查有对有错，必须本地复核。** 本项目里外部审查给出过 `policy.rs 仍是 Local(LazyModel)`、`已完成 21 项测试` 等与源码相反的判断；也给出过 `ensure_states()` 固定 ID、`[0,0]` 半注册这些**正确且关键**的发现。做法：每个指控都回到源码或 `cargo tree` 证实/证伪，再决定采纳还是反驳。
2. **"看起来合理但绝对不能做"比"怎么做"更有价值。** §11 的 30 条禁止模式是防止后续迭代重新引入同类 bug 的主要手段。
3. **文档要分层。** 执行契约（§0–§17）在前，考据（Appendix）在后。考据不能删——它用于反驳错误指控；但不能混在契约里，否则执行者会重新研究一遍。

### 17.6 剩余路线图

| 项 | 状态 |
|---|---|
| **crates.io 发布** | ⏳ 见 §18.3 —— 原「等 rig-run 发布」策略**已作废**（rig-run 被删除），改走 `rig-agent --no-default-features` |
| 版本号 | 直接 **0.3.0**（PR-B 整体已是 breaking），**不要**发中间兼容版 |
| I25 措辞 | 实现是 resolution-time validation，规格应同步改口径（非代码缺陷） |
| 流式 delta（P2-1） | 注意 candle `max_concurrent_requests`=1、stream channel 容量 8 |
| RunSpec 序列化（P2-2） | run 存盘/恢复 |
| Telemetry（P2-3） | 填 `Telemetry` 预留位 |
| Remote / Human driver（P3） | 契约已就绪，`capability` 已字段化，加实现即可 |
| Spark-X2.5 等非 stock 架构 | **不要**给 candle 提架构（量级以周计）。用 `LazyModel` 档位塞 Ollama/vLLM 端点 |

---

## §16 v13：DriverId 覆盖 bug 修复顺序（🔴 当前第一优先级）

### 16.1 已核实的三处固定身份（源码实证）

```rust
// rig/driver.rs
pub const RIG_DRIVER_ID: DriverId = DriverId("rig");

impl Driver for RigDriver {                       // 泛型被抓取工具折叠，实为 RigDriver<M>
    fn id(&self) -> DriverId { RIG_DRIVER_ID }                    // ← 违规 1：固定
    fn capability(&self) -> EscalationTarget { EscalationTarget::Local }  // ← 违规 2：硬编码
    fn submit(&self, ctx, _bus) -> .. {
        self.jobs.send(ModelJob::Attach { run, epoch, model: Arc::clone(&self.model) })
    }
}

// rig/agent.rs
fn ensure_states(world: &mut World) {
    // ...
    if esc.driver_id == RIG_DRIVER_ID && state.is_none() => Some((run, esc.clone()))
    //                  ^^^^^^^^^^^^^ 违规 3：按固定 "rig" 认领 run
}
```

### 16.2 为什么「只改 `Driver::id()`」会得到假绿

```
DriverRegistry:  tier 0 → "rig-candle"   tier 1 → "rig-openai"     ✅ 解析正确
EscalationState.driver_id = "rig-candle"
ensure_states(): "rig-candle" == "rig"?  → false → 不认领
结果：注册正确、执行不发生、run 永久停在 Escalating
```

这正是 §6「绝不静默悬挂」要防的形态。所以 **I30 必须与显式 ID 同批修**。

### 16.3 第二个固定身份：`capability()` 硬编码 `Local`

GPT 未发现，但属同一类 bug 且**后果更隐蔽**：

- `policy.tiers = [Local, Local, Remote]` 时，tier 2 是 `Remote`
- 用 `RigDriver<OpenAI>` 绑 tier 2 → `capability()` 返回 `Local` → I25 校验 `Local != Remote` → `DriverError::Policy`
- **结果：远端模型永远无法通过 RigDriver 注册**

修完 ID 后这条会成为下一个红测。所以 `capability` 必须是 `RigDriver` 的**构造期字段**（或由注册 API 传入），不能是常量。

### 16.4 执行顺序（红测先行）

```
① T2 红测（先写，不修代码）
     FakeCandleModel / FakeOpenAiModel 两个不同类型
     tier 0 → candle、tier 1 → openai
     断言「执行者身份」而非仅断言输出文本不同（见 16.5）
     → 预期失败，证明 ID 覆盖

② 一次性修 identity（三处一起，缺一即假绿）
     RigDriver<M> { id: DriverId, capability: EscalationTarget, model: Arc<M>, jobs }
     Driver::id()         → self.id
     Driver::capability() → self.capability        // 否则 tier=Remote 永远注册不上
     ensure_states::<M>() → esc.driver_id ∈ RigDriverIds<M>   // per-M 隔离（I31）

③ 重构 Registry API：register() 与 bind_tier() 必须彻底拆开（见 16.5a）
     register(driver)          -> Result<_, DuplicateDriverId>      一次
     bind_tier(tier, id)       -> Result<_, DuplicateTierBinding>   多次
     rebind_tier(tier, id)     -> 显式覆盖（留给 P3 动态 provider）
     register_for_tier() 降级为便捷函数，不再承担核心语义

④ T2 绿

⑤ RigModelInbox<M> get-or-create + rig_step_system::<M> 去重 + RigDriverIds<M> 去重登记
     同 M + 多 ID 必须合法（I32），共用一个 inbox 与一个 system

⑥ T3-A：同 M + 两个 DriverId + tier[0,1] → 一个 inbox、一个 system、两档都可执行
⑦ T3-B：不同 M + 不同 ID → 两套 inbox/worker，history 不串
⑧ T1：单模型完整路径
⑨ T4：stale epoch / cancellation / attempt snapshot（I28）
```

### 16.5 T2 必须用两个不同类型 + 必须断言身份

若用 `FakeModel("candle")` / `FakeModel("openai")` 两个**实例**，它们是同一 Rust 类型 → 第二次 `insert_resource(RigModelInbox::<FakeModel>)` 覆盖同一个 inbox、并重复注册 `rig_step_system::<FakeModel>`。这样红测失败**归因不清**（分不清是 ID 冲突还是泛型资源冲突）。必须是两个 `impl CompletionModel` 的类型。

**断言必须是「执行者身份」而非「输出文本」**：

```
✅ tier 0 execution identity == rig-candle
   tier 1 execution identity == rig-openai
❌ 仅断言两段输出文本不同
```

否则两个 fake 恰好返回不同文本，会掩盖「实际都由后注册模型执行」这个真 bug。

### 16.5a 为什么 `register()` 与 `bind_tier()` 必须拆开（v14 关键修正）

当前 `register_for_tier()` 的实现：

```rust
let id = driver.id();
self.register(driver);   // ← 每次都调
self.map_tier(tier, id);
```

**若只把 `register()` 改成「重复 ID 即 Err」而保留这个结构，那么：**

```
register_for_tier(driver, 0)   →  register OK,  bind tier 0
register_for_tier(driver, 1)   →  register 遇重复 ID → Err   ← 第二次必然失败
```

**这会直接杀掉「同一模型绑定多个 tier」这个已确认合法的 T3 路径。**

正确结构是**一次 register、多次 bind**：

```rust
// register_with_model<M> 的最终形态
let inbox  = get_or_create_inbox::<M>(app);           // I32
let driver = RigDriver::new(id, capability, Arc::clone(&model), &inbox);

registry.register(driver)?;                           // 一次
for tier in tiers { registry.bind_tier(tier, id)?; }  // 多次

rig_driver_ids::<M>(app).insert(id);                  // I31
ensure_rig_step_system::<M>(app);                     // 只注册一次
```

三层关系钉死（I32）：**一个 `M` → 一个 inbox + 一个 system；一个 `DriverId` → 一个 Driver 实例；一个 Driver 实例 → 可绑多个 tier。**

### 16.6 剩余待办（修完 16.4 之后）

| 项 | 优先级 | 说明 |
|---|---|---|
| crates.io 发布 | ⏳ | 等 rig 0.43 解 git pin（§9.3）；版本号直接 0.3.0，不要发中间兼容版 |
| 流式 delta（P2-1） | 🟡 | 注意 §13.4 两个约束（candle 并发 1、stream channel 容量 8） |
| RunSpec 序列化（P2-2） | 🟡 | run 存盘/恢复 |
| Telemetry（P2-3） | 🟡 | 填 `Telemetry` 预留位 |
| Remote / Human driver（P3） | 🟢 | 需先解 16.3 的 capability 硬编码 |
| candle 并发信号量 | 🟡 | 默认 1，远端可放宽 |

> ⚠️ **抓取工具会把泛型折叠**（`Arc`、`Sender>`、`RigModelInbox` 均显示为无参数形态，其中 `Sender>` 残留的 `>` 是折叠证据；`Arc` 无参数在 Rust 中不可能编译，故可反推泛型确实存在）。`RigDriver` 是否泛型、`rig_step_system` 是否 turbofish，**动手前用本地源码确认**：
> ```bash
> rg -n "struct RigDriver|fn rig_step_system|struct RigModelInbox|fn capability" crates/bevy_needle/src/rig
> ```

---

## §18 上游重构（2026-09-22 核定）：rig-run 解散、rig-ecs 出现

> 本节记录 09-01 起上游的三条重构线，以及它们对 `bevy_needle` 的真实影响。
> **结论：不迁移 rig-ecs。** 理由比"时机不成熟"更硬 —— 见 §18.4。

### 18.1 时间线（已核实）

| 日期 | 事件 | 状态 |
|---|---|---|
| 08-17 | rig **0.42.0** 发布 | 当前最新发布版 |
| 08-21 | #2397 rig-core 去传输层 | merged to `main` |
| 08-22 | #2403 抽出 `rig-run` | merged to `main`（我们 pin 的 commit） |
| **09-01** | **#2432 dissolve rig-run** | **merged**。rig-run **被删除**，`AgentRun` 迁到 `rig_agent::run` |
| 09-03 | #2446 Pre-rig-bevy（rig-bus） | merged to `feat/effect-bus` |
| 09-07 | #2472 依赖清理 | merged |
| 09-15 | **#2529 rig-ecs** | **merged to `main`**（from `ecs/native-rewrite`） |
| 09-22 | 今天 | rig 仍 `0.42.0`，**0.43 未发布** |

### 18.2 🔴 rig-run 已死 —— 原发布策略作废

#2432 原文：

> "rig-run is gone from the workspace, manifests, wasm CI matrix and provider-layout exemptions."
> "rig-run was created after v0.42.0 and **never released**, so **no compatibility re-exports are kept**."

**这意味着 §9.3 的「等 rig 0.43 发布 rig-run 后解 pin」是在等一个永不存在的东西。**

`AgentRun` 现居 `rig_agent::run`，且**仍是 sans-I/O、`Serialize + Deserialize + Clone`**。

**I13 修订**：原写法「依赖 rig-run，不依赖 rig-agent，因为后者吃 tokio」——**理由已作废**。
#2432 明确：

> "`tests/fixtures/agent_run_stepper` … asserts its graph carries **no rig, tokio, reqwest or rmcp**"
> "`rig_agent_carries_no_runtime_or_mcp` gains a **`--no-default-features` row`"

即 **rig-agent 关闭 default features 后是零运行时的**，且有 CI guard 断言（不是承诺）。

**新策略**：依赖 `rig-agent` + `default-features = false`，用 `rig_agent::run::AgentRun`；tokio 由我们自己显式引入供 worker 用。**rig 给协议，我们给运行时** —— 职责更干净。

### 18.3 crates.io 发布阻塞的真正解法

| 方案 | 评价 |
|---|---|
| ~~等 rig-run 发布~~ | ❌ **已作废**，该 crate 永不发布 |
| **迁 `rig-agent --no-default-features`** | ✅ **推荐**。0.43 发布后全是 registry 依赖，阻塞自动解除 |
| 拆 `bevy_needle_rig` companion crate | 备选。可行（`ToolRegistry`/`collect_transcript` 已是 pub） |
| 发版前脚本摘 pin | ❌ 每次手工操作，易错 |

### 18.4 为什么**不**迁移 rig-ecs（四条，按硬度排序）

**① rig-ecs 不 step `AgentRun` —— 它是另一件事**（本次新发现，最关键）

`crates/rig-ecs/Cargo.toml` 实测依赖**只有** `rig-core`（`default-features = false`），
**既不依赖 rig-agent，也不依赖 rig-run**：

```toml
[dependencies]
rig-core = { path = "../rig-core", version = "0.42.0", default-features = false }
bevy_ecs / bevy_tasks / bevy_reflect / bevy_app / bevy_time / bevy_diagnostic  # 全部 workspace
```

它把 agent/run/turn/utterance 全做成 ECS 实体，**自建 agent graph 而不复用 `AgentRun`**。
所以 rig-ecs **不是**「我们 `AgentRun` 用法的新归宿」——迁移等于**放弃 `AgentRun`**，
即放弃 **I4（Bevy Run ⊃ AgentRun）** 与 **I2（Driver ≠ Agent）** 的地基。

**② Bevy 版本锁死互斥**（客观阻塞，已核实）

```toml
# rig workspace Cargo.toml
bevy_ecs / bevy_app / bevy_tasks / bevy_time / bevy_diagnostic = "0.19.1"

# bevy_needle
bevy_app = "=0.19.0"; bevy_ecs = "=0.19.0"; bevy_tasks = "=0.19.0"
```

`=0.19.0` 精确锁与 `0.19.1` **无法统一** → 两份 bevy_ecs → `Component`/`World`/`Schedule` 类型不互通。
这可修（把 bevy_needle 抬到 `=0.19.1`），但**修它就是一次 breaking**，不能顺手做。

**③ rig-ecs 自己也未发布 → 迁移会加重 pin 负担**

workspace 仍 `version = "0.42.0"`（08-17 发布），而 rig-ecs 09-15 才合入 → **不在 0.42.0 里**。
迁移 = 再加一个 git pin，**不但不解阻塞反而多一个**。

**④ 合并仅 7 天，且上游自己刚推翻过一版**

#2529 原文承认它删掉了自己的上一版："its own loop, its own task table, its own registry, its own ordering counter, five phase markers … **is replaced by the Bevy facility that exists for it**"。
上游刚用一次完整重写推翻自己前作 —— **现在跟进 = 赌它第二版稳**。

### 18.5 rig-ecs 的真实价值：验证而非替代

它独立走到了 `driver = system` / `effects = entities` / `one pass per update`，
**正是我们 I16（无 poll、ECS drain）与 I3（工具走 ECS 实体）的形状**。
这是对既有设计的**第三方验证**，不是替换方案。

可借鉴但**不要照搬**的：`checkpoint::{save_world, load_world}` —— 反射式**整世界**存盘，对游戏太重，只作 P2-2 参考。

### 18.6 三阶段路线（采纳）

```
现在（v0.3.0）：不迁移
    保持 Driver / DriverRegistry / EscalationPolicy / epoch / attempt snapshot /
    Needle ToolInvocation —— 这些是 bevy_needle 的业务语义，rig-ecs 没有替我们定义
    尤其：confidence gate / tier / Escalated vs Failed / Needle→Rig escalation

rig 0.43 + rig-ecs 首个正式 release 后：
    做 RigEcsDriver POC —— implements Driver，内部走 rig-ecs graph
    只验证一个问题：能否把一次 DriverAttempt 映射成 rig-ecs run，
                    而不破坏 Run / epoch / tier / cancellation 语义

POC 通过后再决定是否替换 RigDriver<M> 内部实现
    最终形态很可能是：
        bevy_needle 核心 Run / Escalation / Policy
            └─ Driver trait + DriverRegistry
                 └─ RigDriver，implementation = rig-ecs adapter
```

**一条从现在就该守的纪律**：把 `RigDriver` 当**边界适配器**，不再扩大它的自有 runtime 责任。
以后新增功能：

```
confidence / tier / epoch / cancellation / escalation  → bevy_needle
agent graph / effect lifecycle / handler scheduling     → 向 rig-ecs 靠拢
```

### 18.7 迁移待办（现在就记，0.43 出来时不慌）

1. `rig_run::AgentRun` → `rig_agent::run::AgentRun`（**#2432 已删 rig-run，无兼容 re-export**）
2. `rig-core` / `rig-agent` 必须**同一 git revision**（与现 pin 同理）
3. bevy 是否抬到 `=0.19.1`（若未来要碰 rig-ecs，这是前置）
4. `ConversationId` 已是 newtype（非裸 `String`）、`internal_call_id` 已是 `NonZeroU64`（#2419）→ 影响 §5.3 会话桥键设计
5. 可用新能力（#2419）：`ToolCatalog::execute_owned` / `ConversationMemoryExt::load_owned` 等 owned-future 入口 → worker 侧免去 clone-into-async-move；协议面全量 serde → **P2-2 RunSpec 存盘从"不确定"变可行**，且支持中途流式轮次存盘续跑

---

## §19 「大幅精简 / 删除 RigDriver」提案评估（2026-09-22）

> 提案要点：若确认「rig-ecs = Needle 失败后的通用 Agent 承载层」，则
> `DriverRegistry` / `DriverAttempt` / `RigDriver<M>` 属过度设计，
> 应改为 `Needle → EscalationPolicy → Handoff → rig-ecs Agent`。
>
> **裁决：方向可保留为 POC，架构不动。三条具体反对 + 两条采纳。**

### 19.1 前提未成立（提案自己也承认）

提案原文：

> "我还没有看到官方把 `rig-ecs` 明确定义成『Needle failure fallback runtime』；
> 这一点仍应作为你们自己的产品架构决定"

即这是一个**待定的产品决策**，不是上游事实。在它成立前拆掉已验证的
`Driver` 契约（46/32/28 全绿），是把已交付的确定性换成未验证的可能性。

**规则：不为未确认的前提拆除已验证的代码。** POC 通过前不动主线。

### 19.2 ⚠️ 提案自相矛盾：一边说不动 Policy，一边掏空 Policy

- 声明：「能精简的是 Agent/Driver/runtime 层，**不是 `EscalationPolicy` 本身**」
- 紧接着建议：Policy 退化为 `{ enabled, confidence_threshold, total_budget, handoff_timeout }`，**删掉 tier**

删掉 tier 的代价被低估了。tier 不只是"多档路由"，它承载三条硬约束：

| 约束 | 依赖 tier 的哪一点 |
|---|---|
| **API key 红线**（游戏分发到玩家机器） | `OnlineFallback::LocalModelOnly` 按 **capability ceiling** 拒绝 Remote —— 这是**隐私保证**，不是路由便利 |
| **Spark-X2.5 / 非 stock 架构** | 当初的解法是「用档位槽塞 Ollama/vLLM 端点」，需要 tier 的 capability 语义 |
| **I10 tier≠rank** | `[Local(Candle), Local(GPTQ), Remote]` 两个 Local 同 rank；无 tier 则这套语义无处安放 |

若 handoff 变成一个不透明的整体跳转，**bevy_needle 将失去"这条 run 是否离开本机"的控制权**。
对一个要分发到玩家机器上的游戏 crate，这是产品级退化，不是简化。

### 19.3 ⚠️「工具桥会消失」不成立 —— 它只是换了位置

提案认为 handoff 后「rig-ecs 后续 tool calls 不需要回到 Needle」，只在边界转换一次。

但 bevy_needle 的**用户契约**是 `register_tool_handler` + `ToolCallCompleted` 事件，
游戏代码依赖它做诊断/存档/回放。而 rig-ecs **不复用 `AgentRun`**（§18.4① 已核实：
它的 Cargo.toml 只依赖 `rig-core`，无 rig-agent/rig-run），因此它有**自己的** Effect/Handler 派发。

于是只有两条路：

- **(a) 用户为 rig-ecs 重新注册一遍工具** → 同一工具两份注册，漂移风险
- **(b) 仍需 `ToolSpec` → rig-ecs handler 的桥** → 桥没有消失，只是从
  "`PendingToolCall → ToolInvocation`" 变成 "`ToolSpec → rig-ecs handler`"

**无论哪条，复杂度都不会按提案所说"最低"。** 而 I3 已经保证了两个 driver 共用
**同一个** ECS 派发管线 —— 提案所抱怨的"双工具状态机"在当前架构里**本就不存在**。

真正能删的是重复的 **Agent 循环驱动**：`RigModelInbox<M>` / `rig_step_system<M>` /
`RigDriverState` / `ModelJob` / 手动 `AgentRun` 步进。**这部分提案是对的。**

### 19.4 采纳：DriverRegistry 冻结，不再扩展

提案这句是对的，且值钱：

> "继续往 `DriverRegistry` 上堆功能……可能是在优化一个即将被上游 runtime 吞掉的中间层"

**裁决：即日冻结 `DriverRegistry` 的能力面。**

```
✅ 允许：修 bug、补测试、改文档
❌ 不做：remote driver / human driver / provider discovery / dynamic rebinding
```

这些全部推到 POC 结论之后再决定。Cargo feature 也不再新增。

### 19.5 采纳：POC 改名并补验收项

同意改名 `Needle → Rig-ECS Handoff POC`（不是 `RigEcsDriver` —— 后者仍暗示
「Driver 契约保留、只换实现」，而提案的方向是**整个关系改变**）。

在提案 7 条之外，**补四条它漏掉的**，其中前两条是硬门：

| # | 补充验收项 | 为什么必须有 |
|---|---|---|
| **A** | `OnlineFallback::LocalModelOnly` 是否仍成立？handoff 后能否**禁止**云端模型 | API key 红线是产品底线，不能靠 rig-ecs 自觉 |
| **B** | 现有 `register_tool_handler` 注册的工具，能否**无需重新注册**就在 rig-ecs 下执行 | 决定 19.3 是 (a) 还是 (b)，直接决定复杂度估算 |
| **C** | rig-ecs 的 run 能否被 `RunStatus::Cancelled` 正确取消（含 epoch 失效语义） | rig-ecs 拥有循环后，I17/I28 能否映射是**最大未知** |
| **D** | 该 POC 是**减少**还是**增加** git pin 数量 | 若增加，它与解 crates.io 阻塞（§18.3）目标相悖 |

另需验证一个所有权问题：rig-ecs 自称 "a Bevy app now, not a runtime hosted beside one"，
它会注册自己的 systems/schedule。**bevy_needle 也是 plugin —— 谁来拥有 schedule？**
提案的图里两者并列，但没回答这个。

### 19.6 最终形态（若 POC 通过）

```
bevy_needle
  ├─ Run / EscalationPolicy（保留 tier 与 capability ceiling）
  ├─ Needle 快速工具路由
  └─ Handoff ──► rig-ecs Agent（拥有循环、effect、handler）
                      └─ OpenAI / Ollama / Candle

退休：RigDriver<M> / RigModelInbox<M> / rig_step_system<M> / ModelJob / 手动 AgentRun 步进
保留：EscalationPolicy（含 tier）、Run 状态机、ECS ToolInvocation（若 B 判为 (a) 则转为桥）
```

**退役顺序纪律**：先让 POC 跑通**现有测试矩阵**（46/32/28）的等价集合，
再删旧代码。**不得先删后验。**

### 19.7 决策门

```
现在 ──────────────► rig 0.43 发布 ──► rig-ecs 首个正式 release ──► POC ──► 决策
 │                                                                          │
 └─ 冻结 DriverRegistry                                          POC 通过 → 精简
    不动主线架构                                                 POC 失败 → 维持现状
    Policy 保留 tier                                             （当前代码已可用，非阻塞）
```

### 19.8 维护者最终裁决（2026-09-22，覆盖 19.5 的命名与边界表述）

1. **`RunResolution` 是 bevy_needle 相对 `bevy_rig` → `rig-ecs` 谱系的真正原创边界。**
   分段调度骨架借自 `bevy_rig`，但公开 `bevy_rig` API 只有
   `Preparation/Execution/Commit/ToolDispatch/Telemetry` 五段——**`RunResolution`
   是 bevy_needle 专门为 Needle 决策语义开的层**（置信度/tier/fallback
   ceiling/epoch/升级 handoff）。「两个项目恰好同名」的解释力远弱于架构谱系
   （`bevy_rig` → 分段 runtime → `rig-ecs`，作者谱系是**强信号而非迁移证明**）。

2. **架构定位语（立即生效，全文档统一）**：
   > `bevy_needle` 是 **Needle 的 Bevy integration + resolution/escalation
   > layer**；通用 Agent runtime 可以由 Rig / rig-ecs 承载。
   
   不再写「bevy_needle 是 Rig 的另一个 Agent runtime」。真正与 rig-ecs 重叠的
   只有「怎么把一个 AgentRun 每一轮驱动起来」（inbox/step system/ModelJob），
   不是整个 Agent 产品——后者被 I23/I24 有意挡住。

3. **rig-ecs 是 handoff 目标，不是「另一个 Driver 实现」**。不写 `RigEcsDriver`
   —— 那个名字暗示「Driver 契约保留、只换实现」，而真实关系是
   `Driver / Resolution → handoff → rig-ecs runtime`。

4. **反向约束（同 World）**：rig-ecs 自述 "a Bevy app now"（#2529），会注册自己的
   schedule。POC 必须避免「bevy_needle → rig-ecs App → 另一个 World」的嵌套；
   正确形态是**同一个 Bevy World** 内 `bevy_needle(RunResolution)` 与
   `rig-ecs(RigSchedule)` 并列。schedule 归属问题（谁来拥有）列入 POC 必答项。

5. **`EscalationPolicy`（含 tier 与 capability ceiling）不在任何删除树里**。
   即使 POC 通过、`RigModelInbox<M>` / `rig_step_system<M>` / `ModelJob` /
   手动 AgentRun 步进全部退休，Policy 与 Run 状态机仍然保留——
   「这条 run 是否离开本机」的控制权是产品底线。

6. **决策树（最终版）**：
   ```
   现在：保持 v0.3 架构 + 冻结 DriverRegistry + 删除文档里「对齐 bevy_rig」的现代参照意义
      ↓ 等 Rig API / rig-ecs 稳定
   Needle → Rig-ECS Handoff POC（验收门 §19.5 的 4+7 条）
      ├─ FAIL → 保留当前 RigDriver 全家
      └─ PASS → 逐步退休 RigModelInbox / RigDriverState / ModelJob / 手动 AgentRun loop
                （EscalationPolicy 不在删除树里）
   ```

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
