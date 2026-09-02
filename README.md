# bevy_needle

**Bevy ECS 中的本地工具调用模型。** 把 [needle2](https://github.com/cactus-compute/needle)
—— 14 MB、45M 参数、纯本地推理的工具调用引擎 —— 装进 Bevy 的实体组件系统：
provider/agent/tool/session/run 全是实体与组件，指令解析与函数调用就是几个普通系统。
封装精神对齐 [`bevy_rig`](https://crates.io/crates/bevy_rig)。

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

## 目录

- [快速上手](#快速上手)
- [工具声明（解码语法的艺术）](#工具声明)
- [运行生命周期](#运行生命周期)
- [引擎获取与部署](#引擎获取与部署)
- [面向 45M 模型的工具设计经验](#面向-45m-模型的工具设计经验)
- [演示项目：文字操控 univis_ui](#演示项目univis_needle_demo)
- [架构](#架构)

## 快速上手

```toml
[dependencies]
bevy_needle = "0.1"
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
cargo test -p bevy_needle                        # 21 项测试，无引擎即可全量运行
```

### 教学示例（编号即学习顺序，逐行注释，覆盖全部功能点）

| 示例 | 内容 | 需要引擎？ |
|---|---|---|
| `01_anatomy` | 最小闭环解剖：消息→实体→调用→回执→转录，逐行注释 | ❌ mock |
| `02_tools` | schema 全参数类型（枚举/区间/可选）+ handler 三种写法 + 失败路径（业务 Err / panic / 未知工具） | ❌ mock |
| `03_agents` | 多 agent 自动排队、工具集动态增删触发重绑、全部 agent 参数 | ❌ mock |
| `04_lifecycle` | 逐帧观察 run 状态机：RunTurn / in-flight / awaiting / 逻辑取消 | ❌ mock |
| `05_confidence` | 置信度门控三要素：不执行 + RunEscalation + 失败原因；阈值校准方法 | ❌ mock |
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
- **置信度门控**：低于门限的调用**不执行**，发 `RunEscalation` 消息按契约升级。
  注意：本引擎置信度数值波动极大（实测正确调用可低至 0.0002，也可达 0.9+），
  **默认不设门限**，按自家产品实测校准后再启用。
- **引擎单会话**：同一时刻引擎只服务一个 agent；多 agent 的请求在插件内自动排队，
  切换时在轮次边界重绑（`needle_init` 会重置 KV，这是引擎语义）。
- **逻辑取消**：`CancelRun` 丢弃在途结果（引擎调用本身不可中断）。

## 引擎获取与部署

搜索顺序（`discover_library`）：

1. `BevyNeedlePlugin::new(EngineConfig::with_library(path))` 显式路径（**权威**：缺失即报错）
2. 环境变量 `NEEDLE_LIB_PATH`
3. 仓库内 `third_party/needle/<version>/libneedle.so`（`scripts/fetch_engine.sh --wheel` 解出）
4. 可执行文件同目录
5. `~/.cache/cactus-needle/<version>/`（与 Python 绑定共用缓存）

也可以注入自己的后端：

```rust
app.add_plugins(BevyNeedlePlugin::with_backend(MyEmbeddedBackend::new()));
```

### 获取脚本（`scripts/fetch_engine.sh`）

```bash
scripts/fetch_engine.sh                       # 在线下载当前平台引擎 → ~/.cache/cactus-needle/<ver>/
scripts/fetch_engine.sh --platform macos-arm64 --force   # 交叉获取其他平台
scripts/fetch_engine.sh --wheel ./cactus_needle-2.0.3-py3-none-manylinux2014_x86_64.whl
                                              # 离线：从本地 wheel 解压（air-gapped 设备）
scripts/fetch_engine.sh --list                # 全部平台矩阵
scripts/fetch_engine.sh --out ./third_party/needle     # 输出到仓库内（可提交）
```

支持 8 个 wheel 平台（linux/musl/macos/windows × x86_64/arm64），自动检测 libc
种类（glibc/musl），幂等（已存在跳过，`--force` 重取），平台与 wheel 不匹配直接报错。
armv7/riscv64/mipsel/wasm 等上游仅提供 standalone runner（无 wheel），请用
`--features link` 构建期链接。

调优权重（LoRA 微调合并出的 `.cact`，与引擎版本绑定、加载后不可卸载）：

```rust
BevyNeedlePlugin::new(EngineConfig::with_weights("tuned.cact"))
```

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

核心实现逐模块解析见 [`docs/architecture.md`](docs/architecture.md)。

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
├── app.rs               BevyNeedlePlugin：五段调度 + run 状态机 + 优雅降级
├── tool.rs              工具/调用实体 + 纯函数 handler 分发
├── run.rs / session.rs / agent.rs / schema.rs / diagnostics.rs
```

| feature | 默认 | 说明 |
|---|---|---|
| `dlopen` | ✅ | 运行时加载引擎（常规桌面）。关闭后不链接 libloading，必须 `with_backend` 注入 |

## 致谢

- [needle2 / cactus](https://github.com/cactus-compute/needle) —— 14MB 的本地工具调用引擎与 C ABI 设计
- [bevy_rig](https://crates.io/crates/bevy_rig) —— ECS 集成形态的参照系
- [bevy_needle2](../bevy_needle2) —— 本项目的早期姊妹实现；其 NonSend 主线程设计、
  MockBackend 可测试性思想与 unsafe 纪律已被吸收进本仓库
- [univis_ui](https://github.com/univiseditor/univis_ui) —— 演示项目的 UI 框架

## License

MIT OR Apache-2.0。`third_party/` 下的引擎遵循上游 Apache-2.0。
