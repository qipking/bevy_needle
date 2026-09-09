# bevy_needle

[![crates.io](https://img.shields.io/crates/v/bevy_needle.svg)](https://crates.io/crates/bevy_needle)
[![docs.rs](https://img.shields.io/docsrs/bevy_needle)](https://docs.rs/bevy_needle)
[![License](https://img.shields.io/crates/l/bevy_needle)](https://github.com/qipking/bevy_needle#license)
[![MSRV](https://img.shields.io/badge/MSRV-1.98-blue)](https://github.com/qipking/bevy_needle)

**Bevy ECS 中的本地工具调用模型 + 推理升级层。** 把 [needle3](https://github.com/cactus-compute/needle)
（上游 v3.0.x，~35 MB 基础权重 + 3072 维文本向量，纯本地推理的工具调用引擎）
装进 Bevy 的实体组件系统：
tool/session/run 全是实体与组件，指令解析与函数调用就是几个普通系统。
**架构定位**：`bevy_needle` = Needle 的 Bevy integration + **resolution/escalation
layer**（置信度门控、tier 档位、fallback 上限、跨 Driver 升级）；通用 Agent
runtime 可由 Rig / rig-ecs 承载（见下文[升级](#升级escalation置信度不足时换更强推理)）。
MSRV：**Rust 1.98+**。

```
玩家输入 ──► RunAgent 消息 ──► 工作线程 needle_complete（约束解码，JSON 必合法）
        ◄── ToolCallCompleted / ToolCallFailed 消息 ◄── ECS 工具分发
              │
              └─► 游戏 effect 系统突变任意组件（UI/世界/NPC…），结果回喂引擎继续多轮
```

- **零 Python / 零云端**：直接 dlopen 引擎的四个 C 入口（`needle_init` /
  `needle_complete` / `needle_reset` / `needle_load`），与官方 Python 绑定同 ABI。
- **不卡帧**：阻塞解码在独立工作线程串行执行，ECS 每帧只做提交与收割。
- **引擎即插即用**：缺失引擎不是编译错误也不是 panic —— run 会以可操作的原因失败；
  也可用 `with_backend` 注入任意实现（测试用 mock、嵌入式构建期链接等）。
- **可测试**：`MockBackend` 脚本化信封，完整轮次循环在 CI 上无引擎跑通（10 项测试）。
- **升级（Escalation）**：置信度不足时把推理责任交给另一个 Driver——本地小模型
  失败后自动升到更强模型；核心 crate **永远不知道 rig 是什么**（默认构建零 rig /
  零 tokio，见[升级](#升级escalation置信度不足时换更强推理)）。

## 目录

- [快速上手](#快速上手)
- [工具声明（解码语法的艺术）](#工具声明)
- [运行生命周期](#运行生命周期)
- [升级（Escalation）](#升级escalation置信度不足时换更强推理)
- [引擎获取与部署](#引擎获取与部署)
- [面向 45M 模型的工具设计经验](#面向-45m-模型的工具设计经验)
- [演示项目：文字操控 univis_ui](#演示项目univis_needle_demo)
- [架构](#架构)

## 快速上手

```toml
[dependencies]
bevy_needle = "0.2"   # 0.3.0 发布后可跟进（升级能力见下文）
```

```rust
use bevy_app::App;
use bevy_needle::prelude::*;
use serde_json::json;

let mut app = App::new();
app.add_plugins(BevyNeedlePlugin::default());   // 引擎缺失 → Unavailable，不 panic

// 1) 工具是实体：schema 是数据
let tool = app.world_mut().spawn(ToolBundle::new(ToolSpec::new(
    "set_volume",
    "Set the playback volume.",
    ParametersBuilder::new().int_range("percent", 0, 100, "volume percent").build(),
))).id();

// 2) handler 是不需要 World 的纯函数（执行引擎循环里随时可调）
register_tool_handler(app.world_mut(), "set_volume", |call| {
    let percent = call.args.get("percent").and_then(|v| v.as_i64()).unwrap_or(0);
    Ok(ToolOutput::json(json!({ "volume_set": percent })))
});

// 3) agent = system facts + 工具集绑定
let handles = spawn_agent(
    app.world_mut(),
    NeedleAgentSpec::new("console").with_system_facts("device: desktop; locale: en-US"),
);
attach_tool(app.world_mut(), handles.agent, tool).unwrap();

// 4) 提问 = 发消息；游戏系统读 ToolCallCompleted 施加效果
app.world_mut().write_message(RunAgent::new(handles.agent, "set volume to 80"));
```

无头跑通最小闭环：

```bash
cargo run  -p bevy_needle --example 01_anatomy   # 最小闭环：一次指令的完整旅程（mock，无引擎）
cargo test -p bevy_needle                        # 28 项测试，无引擎即可全量运行
```

### 教学示例（编号即学习顺序，逐行注释，覆盖全部功能点）

| 示例 | 内容 | 需要引擎？ |
|---|---|---|
| `01_anatomy` | 最小闭环解剖：消息→实体→调用→回执→转录，逐行注释 | ❌ mock |
| `02_tools` | schema 全参数类型（枚举/区间/可选）+ handler 三种写法 + 失败路径（业务 Err / panic / 未知工具） | ❌ mock |
| `03_agents` | 多 agent 自动排队、工具集动态增删触发重绑、全部 agent 参数 | ❌ mock |
| `04_lifecycle` | 逐帧观察 run 状态机：RunTurn / in-flight / awaiting / 逻辑取消 | ❌ mock |
| `05_confidence` | 置信度门控三要素：不执行 + RunEscalation + Escalated 收尾；阈值校准方法 | ❌ mock |
| `06_sessions` | 转录镜像、手动追加消息、ChatMessageSeq 排序、ResetAgent 语义 | ❌ mock |
| `07_mock_backend` | Mock 三种注入（脚本/dynamic/自定义代理后端）+ 无引擎测试写法 | ❌ mock |
| `08_extraction` | 结构化抽取：单工具 agent 即抽取器，离题输入返回空调用 | ❌ mock |
| `09_diagnostics` | RuntimeDiagnostics 全字段 + 五段调度的全部插入点演示 | ❌ mock |
| `10_external_dispatch` | External 策略：需要 World 的工具、跨帧完成、与 registry 混合分发 | ❌ mock |

除编号示例外，`01` 换一行即可切到真引擎：

```bash
# 把 BevyNeedlePlugin::with_backend(mock) 换成：
app.add_plugins(BevyNeedlePlugin::default());
# 然后运行（需要引擎，见「引擎获取与部署」）：
cargo run -p bevy_needle --example 01_anatomy --features bevy_needle/dlopen
```

## 工具声明

Needle 从工具 schema 编译**字节级解码语法**：模型不可能产出不合 schema 的参数。
用枚举与区间约束获得接近 100% 的参数可靠性：

```rust
ParametersBuilder::new()
    .str_enum("target", &["anim_toggle", "sound_toggle"], "which toggle")  // 枚举
    .boolean("on", "new state")                                            // 布尔
    .int_range("percent", 0, 100, "volume percent")                        // 有界整数
    .optional_string("note", "why")                                        // 可选字段
    .build()
```

也可以直接给 JSON Schema（`ToolSpec::new` 第三参数）。

## 运行生命周期

```text
EngineSync      工具注册表 + 每个 agent 的引擎快照（签名变化 → 下轮自动重绑）
RunPreparation  RunAgent 消息 → run 实体；CancelRun；ResetAgent
RunExecution    收割引擎事件 → 物化 ToolInvocation → 分发执行 → 结果 JSON 回喂 → 下一轮
RunCommit       终态 run 落入会话转录（User/Assistant/Tool 消息实体）
Telemetry       诊断刷新钩子
```

- **多轮回喂**：本轮调用全部终态后，结果数组（错误以 `{"error": …}`）作为下一轮
  `complete()` 输入 —— 与 Python `run()` 完全一致；`max_steps` 封顶。
- **置信度门控**：低于门限的调用**不执行**，run 走 `Escalating → Escalated`
  正常收尾并发 `RunEscalation` 消息（升级契约的接入点；`Failed` 只留给
  引擎/调度错误）。注意：本引擎置信度数值波动极大（实测正确调用可低至 0.0002，
  也可达 0.9+），**默认不设门限**，按自家产品实测校准后再启用。
- **引擎单会话**：同一时刻引擎只服务一个 agent；多 agent 的请求在插件内自动排队，
  切换时在轮次边界重绑（`needle_init` 会重置 KV，这是引擎语义）。
- **逻辑取消**：`CancelRun` 丢弃在途结果（引擎调用本身不可中断）。

## 升级（Escalation）：置信度不足时换更强推理

0.2.0 把「升级」钉成契约：**低于置信度门限的调用不执行**，run 进入
`Escalating`（中间态）。0.3 补上「谁来接管」——Driver 抽象：

```text
置信度门控 → Escalating{tier} → DriverCoordinator → DriverRegistry（tier → DriverId）
                                                        │
                                    ┌───────────────────┴───────────────────┐
                                    ▼                                       ▼
                            MockDriver（escalate）                  RigDriver<M>（rig）
                            脚本化，无 rig 无网络                     AgentRun 手动步进
                                    │                                       │
                                    └──────► DriverEvent 回灌（channel，无 poll）◄──────┘
                                                        │
                          Escalating 之外只有三个出口：Completed / 下一档 / Failed
                          （从未发生过 attempt → Escalated；试过且坏了 → Failed）
```

分层：`escalate` = **能力层**（状态机 + Driver 抽象 + Coordinator，无 rig 依赖）；
`rig` = **实现层**（RigDriver 把 rig-run 的 `AgentRun` 接进来，工具调用强制走
ECS pipeline）。`policy.rs`（`EscalationPolicy`）无 cfg 无 rig 依赖——
"要不要联网"在没有 rig 的时候也能决定。

### 用法 A：MockDriver 跑通完整升级链路（`escalate` feature，无 rig 无网络）

```rust
use std::sync::Arc;
use bevy_needle::escalate::{DriverId, DriverRegistry, MockDriver, MockStep};
use bevy_needle::prelude::*;

// ① 策略：纯决策——是否启用、能力上限、档位表
app.world_mut().insert_resource(
    EscalationPolicy::new()
        .enabled()
        .with_fallback(OnlineFallback::LocalModelOnly)  // 允许 Local，拒绝 Remote
        .with_tier(EscalationTarget::Local),            // tier = tiers 的下标（不是 rank！）
);

// ② tier 0 → MockDriver：低置信度 run 会被它接管并以脚本输出完成
app.world_mut().resource_mut::<DriverRegistry>().register_for_tier(
    Arc::new(MockDriver::new("mock").with_script(vec![
        MockStep::succeed("escalated by mock"),
    ])),
    0,
).expect("driver id 唯一");
```

四百帧内：低置信度调用 → `Escalating{0}` → coordinator 提交 → mock 回灌
`Succeeded` → run `Completed`，文本即脚本输出——且低置信度的工具调用
**从未执行**（门控契约）。

### 用法 B：RigDriver 接入真实模型（`rig` feature）

```rust
use bevy_needle::escalate::DriverId;
use bevy_needle::policy::EscalationTarget;
use bevy_needle::rig::register_with_model;

// 启动期构造模型——秒级加载（candle 权重）不进升级路径；构造失败就不注册，
// 该 tier 无 driver → 正常收尾 Escalated（"没试"），绝不是 Failed。
let candle: Arc<MyCandleModel> = load_candle_model_at_startup();

// 身份与能力都是显式声明（I30/I25）：
//   - DriverId 重复注册 → RegistryError::DuplicateDriverId（绝不静默覆盖）；
//   - capability 必须与所绑 tier 的 EscalationTarget 一致（Remote 模型声明
//     Local 会让该 tier 永远不可用——注册期就拦下，不是运行期惊喜）；
//   - 一个实例可绑多个 tier；同一 M 多次注册复用同一 inbox/worker（I32）。
register_with_model(
    &mut app,
    DriverId("rig-candle"),
    EscalationTarget::Local,
    candle,
    &[0, 1],     // tier 0 与 1 都由这个模型接管
)?;
```

多模型多档（I10 经典反例）——**两个 Local 档用 rank 推 tier 会坍缩**，
按档位下标绑定即可：

```rust
register_with_model(&mut app, DriverId("rig-a"), EscalationTarget::Local,  candle_a, &[0])?;
register_with_model(&mut app, DriverId("rig-b"), EscalationTarget::Local,  candle_b, &[1])?;
register_with_model(&mut app, DriverId("rig-c"), EscalationTarget::Remote, openai,    &[2])?;
```

Registry API（I29 事务语义）：

| API | 语义 |
|---|---|
| `register(driver)` | 新 id 注册；重复 id → `DuplicateDriverId`（**绝不覆盖**） |
| `bind_tier(tier, id)` | 绑定；该 tier 已绑别的 id → `DuplicateTierBinding`；不做幂等 |
| `rebind_tier(tier, id)` | 唯一显式覆盖路径（动态换 driver 用） |
| `register_for_tier(d, tier)` | 便捷组合，幂等：同实例同 tier → no-op；同实例新 tier → 仅绑定 |
| `precheck_register_and_bind` | 整体预检（含 tiers 入参自身重复检查 `[0,0]` → `DuplicateTierArgument`）——失败零残留 |

取消语义（I28）：`CancelRun` 后取消路由到 **attempt 创建时刻 resolve 的执行者
实例**（`EscalationState.resolved` 快照），registry 后续 rebind 不劫持在途
attempt——旧响应也因 epoch 失效被丢弃。

### 终态归属（一句话判据）

> **「没试 / 不许试」→ `Escalated`；「试过且坏了」→ `Failed`；「用户喊停」→ `Cancelled`。**

| 情形 | 终态 |
|---|---|
| 无可用 driver / policy 不许 / capability 不匹配（未发生 attempt） | `Escalated`（正常收尾，不是失败） |
| driver 执行失败 / 模型错误 / 档位耗尽且末档执行过 | `Failed` |
| `CancelRun` | `Cancelled`（epoch 失效，旧响应不复活） |

### 架构边界（维护者定位，2026-09-22）

`bevy_needle` **不是** Rig 的另一个 Agent runtime。五段调度骨架借自 `bevy_rig`，
但 **`RunResolution` 是本项目真正的原创边界**——置信度、tier、fallback 上限、
epoch、升级 handoff 是 Needle 专属决策语义，通用 Agent runtime 由 Rig /
rig-ecs 承载。相应地：

- `rig-ecs`（上游已合入 main）是**handoff 目标 / Agent runtime**，不是「另一个
  Driver 实现」——未来关系是 `RunResolution → handoff → rig-ecs`，且必须在
  **同一个 Bevy World**（不嵌套第二个 App）；
- `DriverRegistry` 能力面已冻结：修 bug/补测试可以，remote/human driver、
  provider discovery 全部等 `Needle → Rig-ECS Handoff POC`（含 capability
  ceiling / 工具免重注册 / Cancel 语义映射 / pin 数四条硬验收）结论后再议；
- **`EscalationPolicy`（含 tier 与 capability ceiling）不在任何删除树里**——
  对分发到玩家机器的游戏，`LocalModelOnly` 是隐私红线，不是路由便利。

设计全景（不变量 I1–I32、RPITIT 定案、判定核心同源等）见
[`crates/bevy_needle/docs/architecture.md`](crates/bevy_needle/docs/architecture.md)
与执行规格 [`crates/bevy_needle/docs/rig-driver-升级计划.md`](crates/bevy_needle/docs/rig-driver-升级计划.md)
（§18 上游核定 / §19 handoff 提案裁决 / §17 阶段总结）。

## 引擎获取与部署

搜索顺序（`discover_library`，needle3 代际）：

1. `BevyNeedlePlugin::new(EngineConfig::with_library(path))` 显式路径（**权威**：缺失即报错）
2. 环境变量 `NEEDLE_LIB_PATH`
3. `NEEDLE_ENGINE_DIR` 目录下的 `libneedle3.so`
4. 仓库内 `third_party/needle/3.0.1/libneedle3.so`（随仓库分发；获取脚本见下）
5. 可执行文件同目录
6. `~/.cache/cactus-needle/v3/3.0.1/`（与 Python 绑定 v3 分轨共用）

获取 needle3 产物（库 1.2MB 随仓库分发；基础权重 `needle3.cact` 35MB 不进
git，用脚本拉取）：

```bash
third_party/needle/3.0.1/fetch.sh
# OK third_party/needle/3.0.1/needle3.cact (35335380 bytes)
```

也可以注入自己的后端：

```rust
app.add_plugins(BevyNeedlePlugin::with_backend(MyEmbeddedBackend::new()));
```

### 获取引擎

引擎以各平台预编译产物（`libneedle.{so,dylib,dll}`）随仓库 `third_party/needle/<version>/`
分发，或从官方 cactus_needle wheel 中解出放到 `~/.cache/cactus-needle/<version>/`
（与 Python 绑定共用缓存路径）。

支持 linux/musl/macos/windows × x86_64/arm64 等桌面平台；armv7/riscv64/mipsel/wasm 等
上游仅提供 standalone runner（无 wheel），请用构建期链接（`with_backend` 注入）替代
运行时 dlopen。

调优权重（LoRA 微调合并出的 `.cact`，与引擎版本绑定、加载后不可卸载）：

```rust
BevyNeedlePlugin::new(EngineConfig::with_weights("tuned.cact"))
```

### needle3（唯一维护代际）

本 crate **只维护 needle3**（上游 v3.0.x；needle2 已停止支持）。needle3 与
needle2 的引擎面差异：库文件名带代际后缀（`libneedle3.so`）、缓存按代际分轨
（`~/.cache/cactus-needle/v3/3.0.1/`）、**基础权重 `needle3.cact` 不打包进
库**（首次 bind 前自动 `needle_load`，在 `needle_init` 之前——与上游
`_bind → _load_base → needle_init` 顺序一致）、新增 `needle_embed` 符号
（文本 → 3072 维 f32 向量）与 confidence head。`.cact` 档案带 generation
tag——v3 权重与 v2 引擎互不兼容。

```rust
use bevy_needle::prelude::*;

BevyNeedlePlugin::new(
    EngineConfig::with_library("third_party/needle/3.0.1/libneedle3.so")
        .with_base_weights("needle3.cact"),   // 缺省走发现路径（third_party → 缓存分轨）
);
```

needle3 专属能力：`backend.embed(text)` 返回 3072 维 f32 向量；
`confidence` 可能为 `null`（模型没有 confidence head 时），本 crate 的
`Option<f64>` 天然兼容——门控按「无置信度 = 不触发」处理。信封结构
（`type` / `function_calls` / 工具 JSON）与 needle2 完全一致，工具层零迁移。

## 面向 45M 模型的工具设计经验

（均来自本仓库与 [bevy_needle2](../bevy_needle2) 的实测）

- **枚举 / 区间 / 零参数工具最稳**：`animation_on{}` 一次命中；而
  `set_toggle{target enum, on bool}` 这类组合参数经常失手。宁可拆成两个零参工具。
- **词面对齐**：工具描述与用户指令共享实词（"place a tower on the east plot" ↔
  "Place a building … on a plot"）。任务描述里要嵌入工具动词。
- **错误回灌列出合法选项**（"free plots: north, south, west"）——只报 "occupied"
  时模型会逐字复现被拒参数。
- **工具面控制在 6 个左右**、单指令单意图；>5 个工具时引擎检索头每轮只渲染 top-5
  （功能正常，但选择质量下降）。`tool_index_path` 可持久化 embedding 加速。
- **中文是分布外输入**：工具描述/用户提问用中文会得到空调用 + 低置信度（走升级
  路径而不是硬答）。面向中文用户时保持 schema 英文，应用层做翻译/路由。
- **置信度数值不可直接当分数用**：跨会话波动覆盖两个数量级（见上）。

## 演示项目：univis_needle_demo

核心实现逐模块解析见 [`crates/bevy_needle/docs/architecture.md`](crates/bevy_needle/docs/architecture.md)。

`examples/univis_needle_demo` —— 用文字指令操控
[univis_ui](https://github.com/univiseditor/univis_ui)（SDF 混合空间 UI 框架）：
滑杆、开关、主题、进度条、按钮全部由指令驱动，真实指针交互也回显到对话面板。

```bash
cargo run -p univis_needle_demo --release        # 输入英文指令后回车
BEVY_NEEDLE_AUTOPILOT=1 cargo run -p univis_needle_demo --release   # 自动发 6 条指令 + 截图自检
```

指令示例：`set the volume to 80` / `turn the animation off` /
`set the theme to sunset` / `set brightness to 15` / `make the title green`。

## 架构

```text
crates/bevy_needle/src/
├── ffi.rs               唯一 unsafe 模块：dlopen + 四个 C 入口（符号预解析）
├── ffi_loading_guard.rs dlopen 句柄守卫
├── backend.rs           NeedleBackend trait + DlopenBackend + MockBackend
├── engine.rs            信封类型 / 版本常量 / 库发现（100% 安全代码）
├── engine_index.rs      agent 快照：system + 工具集 JSON + FNV 签名（变更驱动重绑）
├── needle_runtime.rs    工作线程执行桥（TurnJob Arc<str> 零拷贝）
├── app.rs               BevyNeedlePlugin：五段调度 + run 状态机 + finalize 安全阀
├── tool.rs              工具/调用实体 + 纯函数 handler 分发
├── policy.rs            EscalationPolicy（无 cfg 无 rig：要不要联网在无 rig 时也能决策）
├── escalate/            升级能力层（feature=escalate）：Driver 契约 + Coordinator + MockDriver
├── rig/                 rig 实现层（feature=rig）：RigDriver<M> + AgentRun 步进 + 工具桥
├── run.rs / session.rs / agent.rs / schema.rs / diagnostics.rs
```

深度解析见 [`crates/bevy_needle/docs/architecture.md`](crates/bevy_needle/docs/architecture.md)；
Driver/升级执行规格（不变量 I1–I32、六版本收敛路径）见
[`crates/bevy_needle/docs/rig-driver-升级计划.md`](crates/bevy_needle/docs/rig-driver-升级计划.md)。

| feature | 默认 | 说明 |
|---|---|---|
| `dlopen` | ✅ | 运行时加载引擎（常规桌面）。关闭后不链接 libloading，必须 `with_backend` 注入 |
| `escalate` | ❌ | 升级**能力层**：`EscalationPolicy` + Driver 抽象 + `DriverCoordinator` + 事务化 `DriverRegistry`（重复 id/绑零变体拒绝），**无 rig 依赖**——`MockDriver` 无 rig 无网络跑通完整升级链路 |
| `rig` | ❌ | rig **实现层**（含 `escalate`）：`RigDriver<M>` 泛型适配（`AgentRun` 手动步进、工具桥强制走 ECS pipeline、worker 侧唯一 async 点）。⚠️ 上游 `rig-run` 未发布，pin 到 #2403 merge commit；crates.io 拒绝带 git 依赖的发布，0.3.0 发版需临时摘除 pin |

## 致谢

- [needle2 / cactus](https://github.com/cactus-compute/needle) —— 14MB 的本地工具调用引擎与 C ABI 设计
- [bevy_rig](https://crates.io/crates/bevy_rig) —— 分段调度骨架的历史来源
  （`RunResolution` 与升级层是本项目原创；通用 Agent runtime 的现代形态由上游
  rig-ecs 正规化）
- [bevy_needle2](../bevy_needle2) —— 本项目的早期姊妹实现；其 NonSend 主线程设计、
  MockBackend 可测试性思想与 unsafe 纪律已被吸收进本仓库
- [univis_ui](https://github.com/univiseditor/univis_ui) —— 演示项目的 UI 框架

## License

MIT OR Apache-2.0。`third_party/` 下的引擎遵循上游 Apache-2.0。
