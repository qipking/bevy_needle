# Changelog

本文件遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，
版本号遵循 [SemVer](https://semver.org/lang/zh-CN/)。

## [Unreleased]

### Added
- **migration verification（§29.7 任务 D；删除门 G1）**：新增
  `docs/migration-0.2.0-to-0.3.0.md`——① 0.2.0→0.3.0 迁移说明（架构分层读法）；
  ② `RunStatus::Escalating/Escalated` 的显式迁移表（Escalated 语义**保留**，
  载具换为 rig-ecs `Failed(Failure::Cancelled(report))`；Escalating → 宿主
  policy 驱动 `UsesModel` 切换；Cancelled 并入 Failed 成员——`Failure`
  九成员为唯一转义出口）；③ 删除后的 API 替代关系表（legacy 逐项 →
  rig-ecs 注册胶水/执行平面；整体消失清单防漏迁）；
- **CI 覆盖升级（§28.6/§29.9 前置）**：push 触发分支补 `master`；
  `rig` / `rig-ecs` 两档测试与 clippy 进矩阵；I11 零成本断言（
  cargo tree 无 rig/reqwest/tokio/futures）入 CI；
- **LocalModelOnly fail-closed（§29.2 任务 A；v28 审计 A 修正）**：
  注册+分类绑定为同一事务（`register_local_model` / `register_remote_model`
  取代两阶段 `insert_remote`）；SecurityGuard fail-closed 矩阵
  （Local→allow / Remote→deny / **unclassified→deny**，默认视为 remote）；
  E2E ×2（已登记 remote deny 回归 + unclassified deny 新增缺口的
  fail-open 修正）；单测矩阵三类全覆盖。
- **worker shutdown / join（§29.3 任务 B）**：stop accepting → drain /
  discard → join worker 显式生命周期；`shutdown()` 幂等（后续 submit
  立即以 `Transport("worker shut down")` 回执，不排队不悬挂）；`join()`
  显式拒绝 "sender still live"（不阻塞）；Drop 只停收不 join
  （"何时确信线程真的结束"由显式 shutdown 回答）。测试 ×4（幂等+停收 /
  drain / join 前拒绝 / 在途解码等待交付）。
- **G2-B2 正向 E2E（§29.4 任务 C）**：轨道修形为 rig-ecs 正典
  world-served tool——`register_open(tool:<name>, Tool family)` + 宿主
  系统提交 `WorldOutcome`（CONTRACT §8.3）；`register_world::<Custom>`
  不会进广告面（Tool 家族过滤）故废弃。E2E 四方断言：World 系统真正
  修改 World（ClipSelection）→ WorldOutcome 提交 → 结果 JSON 回喂
  第二轮 → 内容图 `ToolResult` part 携带规范 call id
  （`needle-call-<seq>`）。
- **worker 唤醒事件驱动化（升级计划 §27.5 ①）**：`WaitSlot` 去 `Condvar` →
  `futures::task::AtomicWaker`；`WaitFuture::poll` 改标准两步式（取 → 注册
  waker → 复查），worker 落回原子唤醒——消除"每 poll spawn 睡眠线程推进"的
  挂死隐患（执行器不再重试轮询即永久 Pending）与每轮 poll 的线程开销；
- **canonical call_id（§27.5 ③ / I18 v27）**：`codec::call_id` 单点 mint
  （`needle-call-<seg>`，model 实例级单调序）——修复旧 per-reply 索引 id
  跨轮碰撞；rig `CallId` 与两侧同值往返；
- **置信度门控接进 rig-ecs 执行路径（§27.5 ④ / §27.4）**：e2e 按 effect 计数
  验收（低置信度 → 零 ToolEffect / 零 WorldEffect / 回调零执行；高置信度 →
  恰 1 个 ToolEffect；respond 轮永不门控）；`ConfidenceGate` 脱离 legacy
  `EscalationPolicy`（阈值注册期给定）；
- **G2-B 双轨工具执行（§27.5 ⑤ / §27.3）**：B1 `register_tool_fn`（宿主直注
  rig 原生 `ToolFn`，删除借道 legacy `ToolHandlerFn` 的
  `Entity::PLACEHOLDER + 空 call_id` 违规桥接）；B2 `register_world_tool::<E>`
  （`Asked<E>` → Bevy system → `Answer<E>`）；
- **NeedleSecurityPolicy 薄安全层（§26.2/§26.3/§26.6）**：`security.rs`
  （`NeedleSecurityPolicy` / `SecurityGuard` / `security_guard` 护栏系统挂
  RigSchedule 的 Select 后 Assemble 前）+ LocalModelOnly E2E（已注册 remote +
  显式选中 → `Failed(Cancelled)` 安全原因、remote 零 served、不误伤本地 run）；
- **运行期取消 E2E（§26.7 Cancellation 行）**：在途取消 → 放行迟到完成 →
  `Failed(Cancelled)` 保持、零答案提交；
- **legacy 冻结（§26.4 / §27.5 ⑦）**：Agent/Run/Tool runtime 定性为
  temporary migration fallback；lib.rs 落冻结纪律；`src/rig/` 零 legacy 依赖
  审计通过。删除门五项中 migration verification 未完成 → 不删除。
- **breaking：Rig 0.44 适配（升级计划 v23，`rig` / `rig-ecs` feature）**：
  - 依赖切到 registry 发布版 `rig-core =0.44.0` + `rig-ecs =0.44.0`（删除
    git-pin 的 rig-run 前提——I13：上游已删除 rig-run，v23 禁止以其为实现
    前提）；bevy 统一 0.19.1（rig-ecs 0.44 的 workspace 依赖精确版本段，
    避免两个 bevy_ecs 版本并存导致 World 类型不互通）；
  - `src/rig/` 重写为 Rig 0.44 契约面：`model.rs`（`Needle3Model` =
    `Model<Needle3Wire, Needle3Transport>`，v23 §7.1 **禁止旧
    `CompletionModel` API**）、`codec.rs`（纯数据转换：历史尾部判定
    §10 / function_calls→ToolCall §6 / 工具结果数组语义 §11）、
    `worker.rs`（独立 Needle 会话 worker，I1/I22/I27——一个 Rig Agent =
    一个 Needle 会话 = 一个 worker，升级计划 §13）、`adapter.rs`（经上游
    `ModelAdapter` 自动提供 `Serve`——**不自写 `impl Serve`**）、
    `gate.rs`（置信度门控 `Intercept`：拒绝点在 tool materialise 之前，
    §12.3 禁止伪 gate）、`host.rs`（rig-ecs host：`Handlers::register`
    模型/工具 + Agent/Grant/UsesModel 装配）；
  - feature 拆分：`rig-ecs = ["rig", "dep:rig-ecs"]`（宿主可选接入
    rig-ecs 运行时；默认构建零成本不变，`cargo tree --no-default-features`
    无 rig/reqwest/tokio/futures）；
  - 旧 rig-run 适配层（`RigModelInbox` / `rig_step_system` / `RigDriverState`
    / `AgentRun` 手动步进 / `RigSessionBridge` / `PortableDynamicTool` 绊线）
    按 I13/v23 裁决删除——rig-ecs 0.44 的原生 Run/Effect/Tool batch 接管；
  - 验收：G1（model adapter e2e 4 测试）、G2（rig-ecs host 闭环 2 测试）、
    G3（置信度门控 4 测试）、G4（LocalModelOnly 拒绝已注册 Remote driver）、
    G5（取消+迟到结果不复活 run）、G6（同模型双 run 不串话）、G7（40 轮
    连续稳定）；全档 feature 回归零失败。

### Added
- **breaking：停止支持 needle2，只维护 needle3（v14.5）**：
  - `EngineGeneration` 收敛为单一 `Gen3`（枚举保留给未来代际）；
    `EngineConfig` 去掉 `with_generation`，新增 `with_base_weights`
    （needle3 权重不打包在库内，首次 bind 前自动 `needle_load`）；
  - 库文件名 `libneedle3.so`、缓存分轨 `~/.cache/cactus-needle/v3/3.0.1/`、
    `needle_embed` 符号为必需（文本 → 3072 维向量 `backend.embed()`）；
  - 真机全链冒烟转正为测试（`needle3_real_smoke`：open → load base →
    init → complete → embed → reset，third_party/needle/3.0.1/ 有产物时跑）；
  - 迁移：`with_generation(Gen3)` 调用删除即可；`.cact` 权重必须用
    needle3 版（generation tag 校验，跨代不兼容）。
- **needle3 代际适配（v14.4，上游 needle v3.0.x 已发布）**：
  - `EngineGeneration`（Gen2/Gen3）+ `EngineConfig::with_generation` /
    `with_base_weights`——默认 Gen2，0.2.x 行为完全不变；
  - Gen3：库文件名带代际后缀（`libneedle3.so`）、缓存分轨
    （`~/.cache/cactus-needle/v3/3.0.1/`）、**首次 bind 前自动加载基础权重
    `needle3.cact`**（base → 调优，均不可卸载；缺失报可操作错误并提示
    `with_base_weights`）、新增 `needle_embed` 可选符号与 `embed()` 文本向量
    API、支持 confidence head（v3 的 `confidence` 可为 null，既有
    `Option<f64>` 天然兼容）；
  - `.cact` 档案带 generation tag——v3 权重不能喂 v2 引擎，反之亦然
    （上游按 tag 分轨，本 crate 靠显式代际声明避免误路由）；
  - 发现路径代际感知：`discover_library_for(gen, path)`；
  - 测试 +5：代际常量/库文件名/缓存分轨/打开响亮失败/Gen2 默认不变。
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

### Fixed
- **v14.3（第二轮外部复核落地）**：
  - `RegistryError::DuplicateTierArgument`：`register_with_model` 的 tiers
    入参自身重复（如 `[0, 0]`）现在显式报错——此前 precheck 通过而 mutation
    中途失败会留下半注册状态（外部审阅实证的反例）；
  - **语义单一事实来源**：抽出 `judge_register_and_bind` 判定核心，
    `register_for_tier` 与 `precheck_register_and_bind` 共用同一判定，
    杜绝「precheck 说 OK / mutation 说 Err」的结构性漂移；
  - 三层测试：registry 单测（`[0,0]` 零变更 / 语义一致性）+ 集成
    （`[0,0]` 零残留：driver/tier/inbox/身份集合全不登记）；
  - 事实澄清：v14.2 的 `register_for_tier` 事务性与幂等 no-op **已实现**
    （`git show` + 实证测试；审阅引用的半注册代码在提交中不存在，
    系 GitHub raw 缓存 serving 旧版）。
- **v14.2（外部审阅修复，commit 9c0b28f5 逐项复核后）**：
  - **组合注册事务性（P0-1）**：`register_for_tier` 改为校验先行（身份冲突与
    tier 冲突全部通过才发生任何变更，语义表见 API 文档）；新增
    `DriverRegistry::precheck_register_and_bind`，`register_with_model` 的
    preflight 提前到一切副作用之前（inbox spawn / 身份登记 / 系统注册都在
    校验之后）——失败 = 整体零残留。专项测试覆盖「新 driver + 已绑 tier +
    fresh tier」场景（driver 不残留、fresh tier 不绑、原绑定不动、身份集合
    不登记）。
  - **I28 取消侧补全（P1）**：`EscalationState.resolved` 保存 attempt 创建
    时刻 resolve 的执行者实例（`Arc` 快照），`cancel_inflight` 据此路由、
    **不再回查 registry**——rebind 后取消也不会误路由。`RigDriver` 增加每
    实例取消计数（可观测依据）；专项测试：在途 + rebind tier0 → 取消 →
    快照实例计数 +1、当前绑定实例零误伤。
  - **T3-A 补两档真执行（P0-2）**：同 `M` 两 DriverId 下两个 run 分别真实
    执行 tier 0（rig-a）与 tier 1（rig-b），按 `driver_id` 断言身份与模型
    调用次数（此前只执行了 tier 0，两档覆盖名不副实）。
  - 文档：CHANGELOG/规格中的旧 `register_with_model` 签名清理。
- **breaking（升级路径多 driver 正确性，规格 v14 §16）**：`RigDriver::id()` /
  `capability()` 从固定常量改为**构造期字段**（`register_with_model` 显式传
  `DriverId` + `EscalationTarget`）——旧实现下多模型注册互相覆盖（后者偷走
  前者的执行权）、Remote 模型永远注册不上（capability 硬编码 Local）、
  `ensure_states` 按常量认领导致「注册正确、执行不发生」的假绿。
  同批修复 tier 推进时旧 `RigDriverState` 未随 attempt 重建的问题（epoch
  驱动重建，P0-5 补全）。
- `DriverRegistry` 身份冲突显式化（I29）：`register` 重复 id / `bind_tier`
  重复绑定现在返回 `RegistryError` 而非静默覆盖；`rebind_tier` 是唯一显式
  覆盖路径；`register_for_tier` 对同一实例幂等。

### Added
- **`RigDriverIds<M>`（I31）**：按模型类型隔离的 rig 身份集合——
  `ensure_states` / `step_all` 只认领/步进本 M 已登记身份的 run；
  **I32**：同一 `M` 复用同一 inbox/worker/步进系统，一个 Driver 实例可绑多 tier。
- **I28（attempt-stable）**：`resolve` 返回 `Arc` 克隆，registry 后续 mutation
  不影响在途 attempt（门闩模型测试验证在途执行者不被 rebind 劫持）。
- 多模型升级链路 e2e（规格 §16.5 红测转绿）：`FakeCandleModel`（Local，脚本
  失败）→ tier1 `FakeOpenAiModel`（Remote，成功），断言**执行者身份**
  （两模型各恰被调用 1 次 + 最终 `driver_id == "rig-openai"`）。
- **CompletionModel 接线**（规格 v9 §13）：worker 侧唯一 async 点
  （`RigModelInbox<M>::spawn` 线程 + `RigRuntime::block_on`）执行
  `model.completion(req).await`；`CompletionResponse → ModelTurn`（allowed
  集合来自 advertise 面）；全部异常**必须回执**（completion 错误 / 未 Attach
  的 run / 通道断开）——run 决不悬挂。宿主入口
  `rig::register_with_model(app, DriverId, EscalationTarget, Arc<M>, &[tiers])`
  （模型启动期预热完成；身份/能力显式声明，注册整体事务——见下 v14.2 节）。
- **I25（capability 匹配）**：`Driver::capability()` 声明 + coordinator
  解析期校验——capability 与 tier 的 `EscalationTarget` 不一致 → 该 tier
  不可用（`DriverError::Policy`，首档走 Escalated / 链路中跳档），把
  「注册错 tier」从静默放行变显式错误；`DriverError` 补 `Policy` 变体。
- **I26（RigModelInbox 唯一注入点）**：`RigDriverState` 字段私有 +
  只读访问器；`AgentRun::model_response` 类型上只能从 inbox drain 触达
  （stale epoch 丢弃语义有专项测试）。
- 测试：`FakeModel`（手工 `impl CompletionModel`，无网络）端到端驱动
  完整模型调用路径；双档推进（tier0 失败 → tier1 成功，epoch+1）e2e。

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
