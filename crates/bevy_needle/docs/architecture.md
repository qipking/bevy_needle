# crates/bevy_needle 核心实现解析

本文解释插件每个模块的职责、关键设计决策与不变量（invariant）。读完应当能
回答：一轮指令从消息到 UI 效果经历了什么、为什么这样做、哪些改动会破坏正确性。

模块地图（与本文小节一一对应）：

```text
crates/bevy_needle/src/
├── ffi.rs               §1  唯一 unsafe：dlopen + 四个 C 入口
├── ffi_loading_guard.rs §1  dlopen 句柄守卫
├── backend.rs           §2  NeedleBackend trait + Dlopen/Mock 两个实现
├── engine.rs            §3  信封类型 / 库发现（零 unsafe）
├── engine_index.rs      §4  agent 快照与签名（变更驱动重绑）
├── needle_runtime.rs    §5  工作线程执行桥
├── app.rs               §6  插件：五段调度 + run 状态机
├── tool.rs              §7  工具/调用实体 + 纯函数 handler 分发
├── agent.rs             §8  agent 组件与绑定
├── run.rs               §9  run 生命周期
├── session.rs           §10 会话转录
├── schema.rs            §11 schema 校验与构建
└── diagnostics.rs       §12 诊断
```

前置知识 —— 引擎的四个事实（决定了下面所有设计）：

| # | 引擎事实 | 来源 |
|---|---------|------|
| F1 | 进程级**单例**，全局 KV 会话，同一时刻只有一个活跃"绑定" | `needle/__init__.py` 的 `_bind()` |
| F2 | `needle_complete` 是**阻塞**调用（桌面毫秒~秒级） | 同上 |
| F3 | 工具集/system 变更需要重新 `needle_init`，**且会重置会话** | 同上 |
| F4 | 引擎只产出 `function_calls`，**不执行工具**；结果由宿主回喂 | 同上 `run()` |

---

## 1. ffi.rs — 唯一的 unsafe 岛

四个 C 入口与 Python 绑定逐字对应：

```c
int  needle_init(const char *system, const char *tools_json, const char *tool_index_path);
int  needle_complete(const char *text, int max_new_tokens, char *out, int out_len);
void needle_reset(void);
int  needle_load(const char *blob, unsigned long long len);
```

设计要点：

- **符号预解析**：`FfiEngine::open` 时把四个符号 `transmute` 成裸函数指针存进
  结构体，之后每次调用不再走 `libloading::Symbol` 的借用检查。
  SAFETY 论证：`Library` 句柄与指针同结构体共存亡（`_lib` 字段），dlopen 的
  引擎在进程存活期内不卸载 —— 指针的 `'static` 化因此成立。
- **`complete` 返回有效字节数**（`usize`）而不是借用切片：借用会在返回前结束，
  调用方（backend）用 `&buffer[..len]` 解析，生命周期干净。
- crate 级 `#![deny(unsafe_code)]`，本文件与 `ffi_loading_guard.rs` 以
  `#![allow(unsafe_code)]` 局部豁免 —— 任何人新增 unsafe 都会编译失败，
  必须显式开洞并写 SAFETY。
- **`dlopen` feature**：`--no-default-features` 时这两个模块整体不编译，
  crate 不链接 libloading（嵌入式构建期链接流程用 `with_backend` 注入）。

错误约定：所有入口返回 `< 0` 即失败；`complete` 的输出缓冲要求 NUL 结尾，
没有 NUL 视为缓冲不足（`BufferNotTerminated`），**绝不静默截断**。

## 2. backend.rs — 可测试性的支点

```rust
pub trait NeedleBackend: Send + Sync + 'static {
    fn bind(&self, signature: u64, system: &str, tools_json: &str,
            tool_index: Option<&Path>) -> Result<(), NeedleError>;
    fn complete(&self, input: &str, max_new_tokens: u32,
                buffer: &mut [u8]) -> Result<NeedleResponse, NeedleError>;
    fn reset(&self);
    fn load_weights(&self, blob: &[u8]) -> Result<(), NeedleError> { ... }
    fn buffer_size(&self) -> usize { DEFAULT_BUFFER_SIZE }
}
```

两个实现：

- **`DlopenBackend`**：包一层 `Mutex<Option<u64>>` 做"已绑定签名"缓存 ——
  签名相同直接返回；`complete` 失败或 `reset`/`load_weights` 后清空，
  强制下次重绑（F3：引擎状态可能已被污染）。
- **`MockBackend`**：两种模式。`new(vec![...])` 按顺序回放信封 JSON（耗尽后
  返回 `respond`）；`dynamic(closure)` 按输入文本动态生成（如"见到 `[` 开头
  就回收尾"），可精确测多轮回喂。计数器（`bind_count` 等）供测试断言轮次。

调度器只认 trait，所以 §6 的整条 run 状态机可以在 CI 上无引擎跑通
（`tests/turn_loop.rs`，10 项）。

## 3. engine.rs — 信封与发现（零 unsafe）

- `NeedleResponse`：引擎 JSON 信封的强类型视图。`#[serde(default)]` 全覆盖 ——
  引擎未来加字段不破坏解析；`kind` 保持 `String`（未知 `type` 变体不能是解析错误，
  与 Python 绑定的宽容语义一致）。
- `is_call()`：`kind == "call" && !function_calls.is_empty()`。**空调用 `[]`
  是引擎的"离题"契约**（不是错误），必须走正常收尾。
- `discover_library`：显式路径是**权威**的 —— 给了路径但缺失立即报错，不静默
  回退（否则"我明明指定了路径为什么用的是另一个引擎"这类问题无法诊断）。
  环境变量与各目录候选依次尝试，错误信息列出全部尝试过的路径。

## 4. engine_index.rs — 签名与变更驱动重绑

`AgentToolIndex`（EngineSync 阶段重建）为每个 agent 存：

```rust
AgentToolSnapshot {
    signature,          // FNV-1a(system + tools_json + index_path + weights)
    system, tools_json, // 传给 needle_init 的原始材料
    tool_index_path, max_new_tokens, max_steps, confidence_threshold, buffer_size,
}
```

关键决策：

- **签名包含绑定所需的一切**，不包含 confidence_threshold/max_steps（这些是
  宿主侧参数，引擎不需要重绑）。改工具集 → 签名变 → §5 工作线程在下一轮前
  自动 `needle_init`。这就是 F3 的处理：重绑在**轮次边界**，绝不会打断一轮。
- **变更驱动**：EngineSync 只在有 agent/工具增删改时重建（`Added/Changed/Removed`
  过滤），静止场景零成本。
- schema 非法的 agent 不 panic：错误进 `AgentToolIndex.errors`，run 失败时给出
  指向工具名的可操作信息。
- 工具 schema 组装成引擎要的完整形态（`{name, description, parameters}` 数组），
  description 缺失的参数自动规范化（`normalize_tool_schema`）。

## 5. needle_runtime.rs — 工作线程执行桥

```text
ECS (每帧)                          工作线程 (串行)
────────────                        ─────────────────────────
submit_turn(TurnJob) ──► job channel ──► backend.bind(若签名变化)
drain_events() ◄────── event channel ◄── backend.complete(input)   // F2 阻塞
```

- **单工作线程** = 天然串行，完美匹配 F1（引擎单会话），不需要锁与优先级。
- `TurnJob` 字段全部 `Arc<str>`/`Arc<str>`：工具集 JSON 每轮零拷贝（它只在
  签名变化时才有意义，但输入文本每轮不同 —— 用同一种廉价克隆）。
- 输出缓冲由工作线程持有并按需增长（`job.buffer_size`），跨轮复用。
- `NeedleRuntime` 是 `Resource`：`Sender` 克隆安全；`Receiver` 包 `Mutex`
  以满足 `Sync`（std mpsc 的 Receiver 不是 Sync）。
- 通道断开（线程崩溃）会在提交侧返回 `false`，记录到诊断
  `channel_broken` —— 不会静默吞掉 run。

## 6. app.rs — 插件与 run 状态机

### 6.1 插件构建（三分支）

```text
with_backend 注入 ──► NeedleRuntime::new(backend)
dlopen 可用      ──► DlopenBackend + 一次性加载 .cact 权重
引擎缺失         ──► 状态 Unavailable（warn 一次），不插 runtime 资源
```

第三分支下，run 提交时发现无 runtime → 失败原因为**可操作文案**
（"设置 NEEDLE_LIB_PATH / 放置 third_party/… / 用 with_backend 注入"），
绝不 panic、绝不悬挂。

### 6.2 五段调度（MainScheduleOrder 中位于 Update 之后）

| 调度 | 职责 |
|------|------|
| `EngineSync` | 重建 ToolRegistry + AgentToolIndex（变更驱动） |
| `RunPreparation` | `RunAgent` 消息 → run 实体（Queued）；`CancelRun`/`ResetAgent` |
| `RunExecution` | 见下 |
| `RunCommit` | 终态 run 落转录（User/Assistant 消息实体），发 `RunCommitted/RunFailed` |
| `Telemetry` | 诊断刷新钩子（计数由事件系统增量维护） |

`RunExecution` 内部三个链式 SystemSet：

```text
EngineExecutionSystems ──► ToolDispatchSystems ──► RunResolutionSystems
(收割事件+提交turn)        (queue→dispatch→publish)   (结果回喂/收尾)
```

游戏在任意位置插入自己的系统：`.before(EngineExecutionSystems)`、
`.in_set(ToolDispatchSystems)`（External 策略工具）、`.after(RunResolutionSystems)`。

### 6.3 状态机

```text
RunAgent ──► Queued ──submit──► Running[in-flight] ──TurnCompleted──┐
              ▲                                                     │
              │                                     ┌── 无调用 ◄────┤
              │                                     ▼              │
        继续轮（结果回喂）◄── 全部终态 ◄─ RunAwaitingTools ◄───────┘
                             │                    有调用
                             ▼
                    Completed / Failed(置信度门控、max_steps)
```

不变量（改代码前先读这里）：

- **每个 run 同时至多一个在途引擎调用**（`RunEngineInFlight`）——事件回来自动移除。
  破坏它 = 同一 run 的轮次交错 = 会话错乱。
- **`RunTurn` 语义：当前轮索引（0 起）**。spawn 调用时打 `ToolInvocationTurn(turn)`，
  resolve 全部终态后回喂并 `RunTurn += 1`。轮次过滤靠它 —— 跨轮统计会把上一轮
  调用重复计数（真实踩过的坑）。
- **每轮工具调用数**：`RunAwaitingTools.expected`。未知工具**立即**生成 Failed
  invocation（错误照常回喂，模型可自恢复 —— 与 Python `run()` 行为一致）。
- **置信度门控只作用于"有调用"的轮**（Needle 契约：门控的是"是否执行调用"，
  最终答复轮的 confidence 没有意义——实测可低至 0.00）。
- **取消是逻辑的**：`CancelRun` 置状态，在途引擎结果到达后按 run 状态丢弃。

### 6.4 一轮的完整旅程（从消息到 UI）

```text
① write_message(RunAgent)             游戏（Update）
② capture_run_requests                RunPreparation：spawn run + User 转录
③ execute_needle_runs                 RunExecution：snapshot 匹配 → submit TurnJob
④ worker: bind? → complete()          工作线程（阻塞 0.3~1.5s）
⑤ 收割 TurnCompleted                  下一~若干帧
   ├─ confidence < threshold → RunEscalation + Failed（不执行）
   ├─ 无调用                → Completed（reasoning 作摘要）
   └─ 有调用                → spawn ToolInvocation（Queued）+ ToolCallRequested 消息
⑥ dispatch_registered_tool_calls      纯函数 handler 执行（panic 会被捕获为 Failed）
⑦ publish_tool_invocation_results     发 ToolCallCompleted/Failed 消息
⑧ 游戏 effect 系统（Update）          读消息 → 突变组件（UI/世界…）
⑨ resolve_run_tool_turns              本轮全部终态 → 结果 JSON 回喂 → 下一轮或收尾
⑩ persist_completed_runs              RunCommit：Assistant 转录 + RunCommitted 消息
```

注意消息时序：游戏 effect 系统在 **Update** 里读消息，读到的是**上一帧**
RunExecution 写入的（Bevy 消息双缓冲）—— 单帧延迟，换来了调度解耦。

## 7. tool.rs — 工具是数据，执行是消息

- `ToolSpec`（实体）= name + description + parameters(JSON Schema)。
  `ToolRegistry`（EngineSync 重建）按名索引，重名先到先得（确定性）。
- `ToolHandlers`：`Arc<HashMap<name, Arc<dyn Fn(&ToolCall) -> Result<ToolOutput,_>>>>`。
  **纯函数、无 World 访问** —— 因此与线程模型正交，也可以在 worker 侧执行
  （当前实现在主线程 dispatch，为的是游戏 effect 系统能同帧看到状态）。
- `ToolInvocation` 状态机：`Queued → Running → Completed/Failed → Published`。
  分发策略：`RegistryHandler`（内置）或 `External`（游戏自己的系统处理，
  内置分发器跳过 —— resolve 只看终态，不关心谁执行）。
- handler panic 被捕获转成 Failed（错误回喂引擎），一个坏 handler 不会炸掉帧。

## 8-10. agent / run / session

- `NeedleAgentSpec`（参数）+ `AgentToolRefs`（绑定）+ `PrimarySession`（会话指针）
  + `AgentEngineOptions`（weights/tool_index 路径）。绑定是**数据**：attach/detach
  就是 push/remove 实体 id，下一轮自动生效。
- `Run` 组件束：request/owner/session/turn/status + awaiting/in-flight/
  pending-input/last-response/executed-results/note/failure。`RunExecutedResults`
  累积每轮回喂的结果（对应 Python `run()` 的 `results`）。
- `Session`/`ChatMessage`：引擎自持 256-token 滑窗，这里的转录是**镜像**。
  消息带显式 `ChatMessageSeq`（CommandQueue 的实体物化顺序不可靠 ——
  实测 spawn c,b,a 会以 c,b,a 物化），按 seq 排序读转录。

## 11. schema.rs — 约束即正确性

- `normalize_tool_schema`：校验 `type:"object"`、`properties` 是对象、
  `required` ⊆ `properties`。非法 schema 的 agent 在 EngineSync 被记错，不 panic。
- `ParametersBuilder`：`str_enum`/`int_range`/`boolean`/… 生成约束。
  **枚举与区间会编译进解码语法** —— 这是 45M 模型参数接近 100% 可靠的唯一来源
  （实测经验见 README"工具设计经验"）。

## 12. diagnostics.rs — 可观测性

计数器（runs/turns/tool_calls 按结果分列）由各系统**增量维护**；
`last_confidence/decode_tps/prefill_tps/peak_ram_mb` 取最近信封。
`RuntimeDiagnostics::summary()` 一行文本适合 debug 覆盖层。

---

## 性能 notes

- EngineSync 变更驱动：静止场景重建成本为 0（曾经每帧全量重建，已改）。
- TurnJob `Arc<str>`：工具集 JSON 每轮零拷贝。
- 输出缓冲跨轮复用，仅在不足时增长。
- 每帧 ECS 侧成本：两次 drain（各一循环）+ 三个查询 —— 与 UI 规模无关。

## 踩坑记录（写给未来的贡献者）

- **`RunTurn` 语义**：曾把自增放在 spawn 调用处，导致 resolve 的轮次过滤永远
  匹配不上（turn 已 +1 而调用还带着旧轮次号），run 永远卡在 awaiting。
  正确顺序：回喂下一轮时才自增。
- **转录顺序不能用实体 id 排序**：CommandQueue 的实体物化顺序与调用顺序相反
  （实测 spawn a,b,c 物化为 c,b,a），消息必须带显式 `ChatMessageSeq`。
- **外部改 `checked`/`selected_index` 后记得同步 previous 字段**：univis 各
  widget 的 emit 系统按 previous 差异发事件，外部突变不同步会让事件每帧重发。
- **效果系统"回执成功"≠"生效"**：目标实体解析必须报错可见（`resolve_target`
  的 warn），并用组件真值（状态转储）做回归，而不是只看消息回执。

## 已知边界（刻意不做）

- 不做流式：引擎无流式接口。
- 不做引擎侧并发：F1 决定了单线程串行是唯一正确模型。
- 不缓存多套工具集的"热切换"：F3 决定切换必然重置会话，缓存无意义。
- `CancelRun` 不能中断在途解码：引擎不可中断；取消语义 = 丢弃结果。
