//! rig-ecs host e2e——无网络（G2 闸门 + §29 任务 A/C/E 级验收）。
//!
//! 覆盖面：
//! - G2：PendingEffect(Completion) → Needle3Model → EffectOutcome 直连；
//!   Agent 两轮工具循环（call → 执行 → 回喂 → respond → Settled）；
//! - §29.4 任务 C：G2-B2 World 工具轨**正向** e2e（Asked 真产生 → Bevy
//!   system 真修改 World → Answer 真回 rig → call_id 规范往返）；deny 分支
//!   （low confidence → 零 effect）保留在 ④ 组；
//! - §29.5 任务 D：confidence effect 级验收（low→0/0、high→1 exactly、
//!   final response 不阻）；
//! - §29.2 任务 A：LocalModelOnly fail-closed（登记 remote deny 回归 +
//!   unclassified deny 新增）；
//! - §29.6 任务 E：cancellation / 迟到结果不复活；
//! - G6/G7：payload 级隔离断言 + 40 轮稳定。
//!
//! 注册面统一走 §29.2 任务 A 的事务 API：`register_local_model` /
//! `register_remote_model`（注册+分类同事务）。

#![cfg(feature = "rig-ecs")]

use bevy_app::{App, Startup, Update};
use bevy_ecs::prelude::*;
use rig_ecs::bus::Handlers;
use rig_ecs::prelude::*;
use rig_ecs::systems::RunCommands;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bevy_needle::MockBackend;

// ────────────────────────────── 公共夹具 ──────────────────────────────

/// 两轮脚本后端：第一轮 call，第二轮 respond。
fn two_turn_backend() -> MockBackend {
    MockBackend::new(vec![
        json!({
            "type": "call",
            "success": true,
            "function_calls": [
                { "name": "set_volume", "arguments": { "percent": 80 } }
            ],
            "confidence": 0.94,
        }),
        json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": "volume is now 80",
            "confidence": 0.9,
        }),
    ])
}

/// set_volume 工具回调（可选计数器；§27.3 B1 轨——纯函数，无 World 访问）。
fn volume_callback(
    counter: Option<Arc<AtomicU64>>,
) -> impl for<'a> std::ops::Fn(
    &'a mut rig_core::tool::ToolContext,
    Value,
) -> rig_core::wasm_compat::WasmBoxedFuture<
    'a,
    Result<rig_core::tool::ToolOutput, rig_core::tool::ToolExecutionError>,
> + rig_core::wasm_compat::WasmCompatSend
+ rig_core::wasm_compat::WasmCompatSync {
    move |_context: &mut rig_core::tool::ToolContext, args: Value| {
        if let Some(counter) = &counter {
            counter.fetch_add(1, Ordering::SeqCst);
        }
        let percent = args.get("percent").and_then(Value::as_i64).unwrap_or(0);
        Box::pin(async move {
            Ok(rig_core::tool::ToolOutput::json(json!({ "volume_set": percent })))
        })
    }
}

/// ToolCall 家族 effect 的 materialise 计数（§27.4 验收的观测面）。
#[derive(Resource, Default)]
struct ToolEffectCount(u64);

/// World-effect 落到 effect 实体上的计数（`WorldEffect == 0` 断言 + 任务 C 正向计数）。
#[derive(Resource, Default)]
struct WorldQuestionCount(u64);

/// 为具体 WorldEffect `E` 注册 Asked 计数观察者。
macro_rules! watch_world_questions {
    ($app:expr, $ty:ty) => {{
        fn watch(
            added: On<Add, rig_ecs::bus::Asked<$ty>>,
            mut counter: ResMut<WorldQuestionCount>,
        ) {
            let _ = added;
            counter.0 += 1;
        }
        $app.add_observer(watch);
    }};
}

fn watch_tool_effects(
    added: On<Add, rig_ecs::bus::PendingEffect>,
    effects: Query<&rig_ecs::bus::PendingEffect>,
    mut counter: ResMut<ToolEffectCount>,
) {
    let entity = added.event().entity;
    if let Ok(effect) = effects.get(entity) {
        if matches!(effect.kind, rig_core::effect::EffectKind::ToolCall { .. }) {
            counter.0 += 1;
        }
    }
}

/// 通用驱动：跑到任意终态（Settled / Failed / Cancelled 都算到达）。
fn drive_to_terminal(app: &mut App, max_attempts: u64) {
    let mut attempts = 0;
    loop {
        app.update();
        let world = app.world_mut();
        let mut settled = world.query::<&Settled>();
        let mut failed = world.query::<&Failed>();
        if settled.iter(world).next().is_some() || failed.iter(world).next().is_some() {
            return;
        }
        attempts += 1;
        assert!(attempts < max_attempts, "run did not reach a terminal state");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 等待唯一 Settled 答案（失败即 panic 带原因）。
fn await_settled_answer(app: &mut App) -> String {
    let mut attempts = 0;
    loop {
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<(&RunResult, &Settled)>();
        if let Some((result, _)) = query.iter(world).next() {
            return result.0.clone();
        }
        let mut failed = world.query::<&Failed>();
        if let Some(failure) = failed.iter(world).next() {
            panic!("run failed instead of settling: {:?}", failure.0);
        }
        attempts += 1;
        assert!(attempts < 400, "run did not settle in time");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 驱动到「既有 Settled 又有 Failed」双终态。
fn drive_to_two_endings(app: &mut App) {
    let mut attempts = 0;
    loop {
        app.update();
        let world = app.world_mut();
        let mut settled = world.query::<(&RunResult, &Settled)>();
        let mut failed = world.query::<&Failed>();
        let settled_count = settled.iter(world).count();
        let failed_count = failed.iter(world).count();
        if settled_count >= 1 && failed_count >= 1 {
            return;
        }
        attempts += 1;
        assert!(
            attempts < 400,
            "runs did not terminate: settled={settled_count} failed={failed_count}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

/// 哨兵 WorldEffect（27.4 断言 `WorldEffect == 0` 的计数对象）。
#[derive(Component, Debug, Clone, Copy)]
struct WatchQuestions;

impl serde::Serialize for WatchQuestions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serde_json::Value::Null.serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for WatchQuestions {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let _ = serde_json::Value::deserialize(deserializer)?;
        Ok(WatchQuestions)
    }
}

impl rig_core::effect::CustomEffect for WatchQuestions {
    const KIND: &'static str = "bevy_needle.test:watch_questions";
    type Answer = serde_json::Value;
}

// ────────────────────────────── G2 ──────────────────────────────

#[test]
fn needle_model_serves_completion_effect_directly() {
    // bus 直连形态：PendingEffect(Completion) → Needle3Model → EffectOutcome。
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.add_systems(Startup, move |mut handlers: Handlers, mut commands: Commands| {
        handlers
            .register(
                "model:needle3-direct",
                rig_core::serve::adapters::ModelAdapter::new(
                    bevy_needle::rig::NEEDLE_LABEL,
                    rig_core::driver::DynModel::from(
                        bevy_needle::rig::needle3_model(
                            Arc::new(MockBackend::new(vec![json!({
                                "type": "respond",
                                "success": true,
                                "function_calls": [],
                                "reasoning": "hello from needle",
                            })])),
                            "needle3",
                        )
                        .into_inner(),
                    ),
                ),
            )
            .expect("register");
        commands.spawn(rig_ecs::bus::PendingEffect::new(
            "model:needle3-direct",
            rig_core::effect::EffectKind::Completion {
                request: rig_core::completion::CompletionRequest::from(vec![
                    rig_core::message::Message::user("hello"),
                ]),
                stream: false,
            },
        ));
    });
    let mut attempts = 0;
    loop {
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<&rig_ecs::bus::EffectOutcome>();
        if let Some(outcome) = query.iter(world).next() {
            match &outcome.0 {
                Ok(rig_core::effect::Outcome::Completion(response)) => {
                    assert_eq!(response.text(), "hello from needle");
                    assert_eq!(response.raw["reasoning"], json!("hello from needle"));
                }
                other => panic!("unexpected outcome: {other:?}"),
            }
            break;
        }
        attempts += 1;
        assert!(attempts < 200, "effect did not land in time");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn rig_ecs_agent_runs_two_turn_tool_loop() {
    let model = bevy_needle::rig::needle3_model(Arc::new(two_turn_backend()), "needle3");
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.add_systems(Startup, move |mut commands: Commands| {
        // 系统体只 clone 捕获量（FnMut 兼容）；注册与 run 全进队列。
        let queued_model = model.clone();
        let queued_callback: Option<Arc<AtomicU64>> = None;
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                None,
            )
            .expect("register local needle model");
            let tool_entity = Handlers::with(world, |handlers| {
                bevy_needle::rig::register_tool_fn(
                    handlers,
                    "set_volume",
                    "Set the playback volume.",
                    json!({"type":"object","properties":{"percent":{"type":"integer"}},"required":["percent"]}),
                    volume_callback(queued_callback),
                )
                .expect("register tool")
            })
            .expect("bus must be installed");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("editor".to_owned()),
                    rig_ecs::agent::Preamble(Some(
                        "You are an audio editor assistant.".into(),
                    )),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(256)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(4)),
                    rig_ecs::agent::MaxTurns(4),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            world.spawn((
                rig_ecs::agent::Grant(tool_entity),
                bevy_ecs::hierarchy::ChildOf(agent),
            ));
            world.spawn_run(agent, &[], "Calculate 2 - 5.", false, None);
        });
    });
    let answer = await_settled_answer(&mut app);
    assert_eq!(answer, "volume is now 80");
}

// ─────────────────── G6 payload 级断言（§27.6 / I33）───────────────────

#[test]
fn two_runs_same_model_do_not_cross_talk() {
    // I33 / §27.6：G6 验收升级为 **payload 级断言**——捕获引擎实际收到的
    // 每一份输入，断言没有一次请求混入另一个 Run 的 utterance。
    let engine_inputs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = engine_inputs.clone();
    let backend = MockBackend::dynamic(move |input: &str| {
        recorder
            .lock()
            .expect("engine input recorder")
            .push(input.to_owned());
        let answer = match input {
            "first" => "first answer",
            "second" => "second answer",
            other => panic!("engine received unexpected payload: {other:?}"),
        };
        json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": answer,
        })
    });
    let model = bevy_needle::rig::needle3_model(Arc::new(backend), "needle3");
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = model.clone();
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                None,
            )
            .expect("register");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("shared".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(64)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(2)),
                    rig_ecs::agent::MaxTurns(2),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            world.spawn_run(agent, &[], "first", false, None);
            world.spawn_run(agent, &[], "second", false, None);
        });
    });

    let mut answers = Vec::new();
    let mut attempts = 0;
    while answers.len() < 2 {
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<(&RunResult, &Settled)>();
        for (result, _) in query.iter(world) {
            if !answers.contains(&result.0.clone()) {
                answers.push(result.0.clone());
            }
        }
        attempts += 1;
        assert!(attempts < 400, "runs did not settle in time");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(answers.len(), 2, "two runs must each settle: {answers:?}");
    assert!(
        answers.contains(&"first answer".to_string())
            && answers.contains(&"second answer".to_string()),
        "answers must not cross: {answers:?}"
    );

    // Payload 级断言（§27.6 / I33）：每次引擎调用收到的输入必须**恰好是
    // 本 run 自己的 utterance**——不得包含另一 run 的 prompt / tool result，
    // 也不得是历史拼接（§10：不重放整段 history；Needle 会话在引擎内）。
    let recorded = engine_inputs.lock().expect("engine input recorder").clone();
    assert_eq!(recorded.len(), 2, "exactly one engine call per run: {recorded:?}");
    for input in &recorded {
        assert!(
            !input.contains("first") || !input.contains("second"),
            "cross-run bleed in engine payload: {input:?}"
        );
    }
    let mut sorted = recorded.clone();
    sorted.sort();
    assert_eq!(sorted, vec!["first".to_string(), "second".to_string()]);
}

/// G7（升级计划 §17.7 / §28.5 措辞）：重复循环稳定性——同一 handler 的
/// single-active-turn 语义下连续 N 次单轮 run（串行由
/// `ServingPolicy::serial_per_handler` + worker 串行队列共同承担；
/// **不是**"任意多个 Needle model 自然并发"——进程级单会话约束见 worker.rs）。
#[test]
fn repeated_single_turn_runs_are_stable() {
    const ROUNDS: usize = 40;
    let script: Vec<Value> = (0..ROUNDS)
        .map(|round| {
            json!({
                "type": "respond",
                "success": true,
                "function_calls": [],
                "reasoning": format!("answer-{round}"),
            })
        })
        .collect();
    let model = bevy_needle::rig::needle3_model(Arc::new(MockBackend::new(script)), "needle3");
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = model.clone();
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                None,
            )
            .expect("register");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("loop".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(64)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(1)),
                    rig_ecs::agent::MaxTurns(1),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            for round in 0..ROUNDS {
                world.spawn_run(agent, &[], format!("round-{round}"), false, None);
            }
        });
    });

    let mut settled = Vec::new();
    let mut attempts = 0;
    while settled.len() < ROUNDS {
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<(&RunResult, &Settled)>();
        for (result, _) in query.iter(world) {
            if !settled.contains(&result.0.clone()) {
                settled.push(result.0.clone());
            }
        }
        attempts += 1;
        assert!(attempts < 4_000, "loop runs did not settle in time");
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // 顺序不承诺（并发调度），但集合必须精确匹配且无重复。
    let mut expected: Vec<String> = (0..ROUNDS).map(|r| format!("answer-{r}")).collect();
    expected.sort();
    let mut got = settled;
    got.sort();
    assert_eq!(got, expected, "every round must settle exactly once");
}

// ─────────────────── ④ confidence gate（任务 D / §27.4）─────────────────
//
// effect 级验收：
//   low confidence  → ToolEffect == 0，WorldEffect == 0
//   high confidence → ToolEffect == 1（exactly）
//   final response  → 必须正常 settle
// 不得新增第二个独立 confidence gate（本路径唯一 gate = ConfidenceGate）。

#[test]
fn confidence_gate_lifts_zero_tool_effects_on_low_confidence() {
    let callback_count = Arc::new(AtomicU64::new(0));
    // 一次性信封：低置信度 tool call（被拒）。
    let model = bevy_needle::rig::needle3_model(
        Arc::new(MockBackend::new(vec![json!({
            "type": "call",
            "success": true,
            "function_calls": [{ "name": "set_volume", "arguments": { "percent": 1 } }],
            "confidence": 0.4,
        })])),
        "needle3",
    );
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.init_resource::<ToolEffectCount>();
    app.init_resource::<WorldQuestionCount>();
    app.add_observer(watch_tool_effects);
    watch_world_questions!(app, WatchQuestions);
    let count_for_queue = callback_count.clone();
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = model.clone();
        let count_for_queue = count_for_queue.clone();
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                Some(bevy_needle::rig::ConfidenceGate::new(0.85)),
            )
            .expect("register needle model");
            let tool_entity = Handlers::with(world, |handlers| {
                bevy_needle::rig::register_tool_fn(
                    handlers,
                    "set_volume",
                    "Set the playback volume.",
                    json!({"type":"object","properties":{"percent":{"type":"integer"}}}),
                    volume_callback(Some(count_for_queue.clone())),
                )
                .expect("register tool")
            })
            .expect("bus must be installed");
            // B2 World 工具轨也授权（同样绝不允许被走到的另一条路）。
            let world_tool = Handlers::with(world, |handlers| {
                bevy_needle::rig::register_world_tool(
                    handlers,
                    "watch_questions",
                    "Watch world questions (sentinel).",
                    json!({"type":"object"}),
                )
                .expect("register world tool")
            })
            .expect("bus must be installed");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("gated".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(128)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(3)),
                    rig_ecs::agent::MaxTurns(3),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            for tool in [tool_entity, world_tool] {
                world.spawn((
                    rig_ecs::agent::Grant(tool),
                    bevy_ecs::hierarchy::ChildOf(agent),
                ));
            }
            world.spawn_run(agent, &[], "把音量调到 1", false, None);
        });
    });

    drive_to_terminal(&mut app, 300);
    let world = app.world_mut();
    let tool_effects = world.resource::<ToolEffectCount>().0;
    let world_questions = world.resource::<WorldQuestionCount>().0;
    assert_eq!(tool_effects, 0, "低置信度 → ToolEffect == 0（§27.4）");
    assert_eq!(world_questions, 0, "低置信度 → WorldEffect == 0（§27.4）");
    assert_eq!(callback_count.load(Ordering::SeqCst), 0, "工具回调零执行");
    // run 终态：Provider 失败，原因来自门控（不是 Settled）。
    let mut query = world.query::<&Failed>();
    let failure = query.iter(world).next().expect("run must have failed");
    let rig_ecs::agent::Failure::Provider(report) = &failure.0 else {
        panic!("低置信度拒绝必须是 Provider 失败：{:?}", failure.0);
    };
    assert!(
        report.to_string().contains("低于门限"),
        "失败原因应来自置信度门控：{report}"
    );
}

#[test]
fn confidence_gate_passes_exactly_one_tool_effect() {
    let callback_count = Arc::new(AtomicU64::new(0));
    let count_for_queue = callback_count.clone();
    let model = bevy_needle::rig::needle3_model(Arc::new(two_turn_backend()), "needle3");
    let queued_model = model.clone();
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.init_resource::<ToolEffectCount>();
    app.add_observer(watch_tool_effects);
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = queued_model.clone();
        let count_for_queue = count_for_queue.clone();
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                Some(bevy_needle::rig::ConfidenceGate::new(0.85)),
            )
            .expect("register");
            let tool_entity = Handlers::with(world, |handlers| {
                bevy_needle::rig::register_tool_fn(
                    handlers,
                    "set_volume",
                    "Set the playback volume.",
                    json!({"type":"object","properties":{"percent":{"type":"integer"}}}),
                    volume_callback(Some(count_for_queue.clone())),
                )
                .expect("register tool")
            })
            .expect("bus must be installed");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("gated".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(128)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(3)),
                    rig_ecs::agent::MaxTurns(3),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            world.spawn((
                rig_ecs::agent::Grant(tool_entity),
                bevy_ecs::hierarchy::ChildOf(agent),
            ));
            world.spawn_run(agent, &[], "把音量调到 80", false, None);
        });
    });

    let answer = await_settled_answer(&mut app);
    assert_eq!(answer, "volume is now 80");
    assert_eq!(
        app.world().resource::<ToolEffectCount>().0,
        1,
        "高置信度 → ToolEffect == 1（exactly，§27.4）"
    );
    assert_eq!(callback_count.load(Ordering::SeqCst), 1, "工具回调恰好执行一次");
}

#[test]
fn confidence_gate_never_blocks_final_response() {
    let model = bevy_needle::rig::needle3_model(
        Arc::new(MockBackend::new(vec![json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": "all done without tools",
            "confidence": 0.01,
        })])),
        "needle3",
    );
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = model.clone();
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                Some(bevy_needle::rig::ConfidenceGate::new(0.85)),
            )
            .expect("register");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("direct".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(64)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(1)),
                    rig_ecs::agent::MaxTurns(1),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            world.spawn_run(agent, &[], "说一句", false, None);
        });
    });
    let answer = await_settled_answer(&mut app);
    assert_eq!(
        answer,
        "all done without tools",
        "respond 轮必须原样 settle（confidence=0.01 也不阻）"
    );
}

// ─────────────────── ⑤ G2-B2 World 工具轨（任务 C 正向 e2e）─────────────────
//
// §29.4 任务 C：World 系统（可用 Query<&mut World>）真正修改 World →
// WorldOutcome 提交 → Answer 真正回到 Rig（.tool result 由 rig-ecs 以
// wire mint 的 `needle-call-<seq>` call id 落历史）。
//
// 轨道形态（上游 CONTRACT §8.3 的正典 world-served tool）：
//   Handlers::register_open(tool:<name>, Tool family)   ← 广告面（B1 同源）
//   dispatch lands on the effect entity itself          ← "问题"
//   宿主系统读 PendingEffect → 改 World → 插 WorldOutcome ← "答案"

/// World 修改的观测面：宿主系统真正改了什么。
#[derive(Resource, Default)]
struct ClipSelection {
    selected: Option<String>,
    selections: u64,
}

/// B2 轨应答系统：读 world-served effect（ToolCall），**用 World 访问**
/// 修改资源并提交 `WorldOutcome`（§29.4：只有这条轨能证明 execution
/// semantics 真正归 rig-ecs / Bevy World）。
fn answer_select_clip(
    effects: Query<
        (Entity, &rig_ecs::bus::PendingEffect),
        (
            Added<rig_ecs::bus::InFlight>,
            Without<rig_ecs::bus::WorldOutcome>,
        ),
    >,
    mut fixture: ResMut<ClipSelection>,
    mut commands: Commands,
) {
    for (entity, effect) in &effects {
        let rig_core::effect::EffectKind::ToolCall { name, args } = &effect.kind
        else {
            continue;
        };
        if name != "select_clip" {
            continue;
        }
        let clip_id: String =
            serde_json::from_str(args).ok().and_then(|value: Value| {
                value.get("clip_id").and_then(Value::as_str).map(str::to_owned)
            })
            .unwrap_or_default();
        // 真正的 World 修改（资源；应用侧等价于 Query<&mut Selection> 写组件）。
        fixture.selected = Some(clip_id.clone());
        fixture.selections += 1;
        commands.entity(entity).insert(rig_ecs::bus::WorldOutcome::new(
            Ok(rig_core::effect::Outcome::ToolResult {
                result: rig_core::tool::ToolResult::success(
                    rig_core::tool::ToolOutput::json(json!({ "clipped": clip_id })),
                ),
            }),
        ));
    }
}

#[test]
fn world_tool_track_roundtrips_answer_and_call_id() {
    let engine_inputs: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = engine_inputs.clone();
    let backend = MockBackend::dynamic(move |input: &str| {
        recorder
            .lock()
            .expect("engine input recorder")
            .push(input.to_owned());
        if input.starts_with("[") {
            // 第二轮：工具结果数组回喂 → respond。
            return json!({
                "type": "respond",
                "success": true,
                "function_calls": [],
                "reasoning": "clipped and done",
            });
        }
        // 第一轮：call select_clip。
        json!({
            "type": "call",
            "success": true,
            "function_calls": [
                { "name": "select_clip", "arguments": { "clip_id": "clip-7" } }
            ],
            "confidence": 0.94,
        })
    });
    let model = bevy_needle::rig::needle3_model(Arc::new(backend), "needle3");
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.init_resource::<ClipSelection>();
    let queued_model = model.clone();
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = queued_model.clone();
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                None,
            )
            .expect("register");
            let tool_entity = Handlers::with(world, |handlers| {
                bevy_needle::rig::register_world_tool(
                    handlers,
                    "select_clip",
                    "Select a clip in the editor.",
                    json!({"type":"object","properties":{"clip_id":{"type":"string"}},"required":["clip_id"]}),
                )
                .expect("register world tool")
            })
            .expect("bus must be installed");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("editor".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(256)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(3)),
                    rig_ecs::agent::MaxTurns(3),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            world.spawn((
                rig_ecs::agent::Grant(tool_entity),
                bevy_ecs::hierarchy::ChildOf(agent),
            ));
            world.spawn_run(agent, &[], "选中 clip-7", false, None);
        });
    });
    // 应答系统：普通 Update 系统（RigSchedule 之后下一帧可见 InFlight）。
    app.add_systems(Update, answer_select_clip);

    let answer = await_settled_answer(&mut app);
    assert_eq!(answer, "clipped and done");

    let world = app.world_mut();
    // ① Bevy system 真正修改 World。
    let fixture = world.resource::<ClipSelection>();
    assert_eq!(fixture.selected.as_deref(), Some("clip-7"));
    assert_eq!(fixture.selections, 1);
    // ② Answer<E>（WorldOutcome → Outcome）真正回到 Rig：第二轮引擎输入
    //    含结果 JSON。
    let recorded = engine_inputs.lock().expect("engine input recorder").clone();
    assert!(
        recorded
            .iter()
            .any(|payload| payload.contains("clipped") && payload.contains("clip-7")),
        "第二轮回喂必须携带工具结果 JSON：{recorded:?}"
    );
    // ③ 正确 call_id 被保留（I18 v27）：内容图上的 ToolResult part（组件
    // `rig_ecs::agent::content::parts::ContentPart`）必须携带 wire mint 的
    // 规范 call id（`needle-call-<seq>`）。
    let mut found_call_id = false;
    let world = app.world_mut();
    {
        let mut query =
            world.query::<&rig_ecs::agent::content::parts::ContentPart>();
        for part in query.iter(world) {
            if let rig_ecs::agent::content::parts::ContentPart::ToolResult {
                call,
                name,
                is_error,
            } = part
            {
                assert!(!*is_error, "正向轨的工具结果不得是 error");
                assert!(
                    call.wire().starts_with("needle-call-"),
                    "tool result 的 call id 必须是规范形态：{call}"
                );
                assert_eq!(name.as_str(), "select_clip");
                found_call_id = true;
            }
        }
    }
    assert!(found_call_id, "历史必须包含一个 ToolResult part");
}

// ─────────────────── ⑥ LocalModelOnly fail-closed（任务 A）─────────────────
//
// §29.2 语义：
//   classified Local   → allow
//   classified Remote  → deny
//   unclassified       → deny（fail-closed：不需要调用方再登记）

const REMOTE_MODEL_KEY: &str = "model:remote";

/// 远端哨兵模型：被选中即置 served 旗（断言"永不选中"的观测面）。
struct RemoteMock {
    served: Arc<AtomicBool>,
}

impl rig_core::serve::Serve for RemoteMock {
    type Family = rig_core::effect::family::Completion;

    fn descriptor(&self) -> rig_core::effect::HandlerDescriptor {
        rig_core::effect::HandlerDescriptor {
            key: rig_core::effect::HandlerKey::from(REMOTE_MODEL_KEY),
            family: rig_core::effect::FamilyDescriptor::Completion {
                model: rig_core::completion::ModelRef::new("remote-mock"),
                capabilities: rig_core::completion::ProviderCapabilities::default(),
            },
            layers: Vec::new(),
        }
    }

    async fn serve(
        &self,
        _kind: rig_core::effect::EffectKind,
        _dispatch: rig_core::serve::Dispatch,
    ) -> rig_core::serve::Reply {
        self.served.store(true, Ordering::SeqCst);
        let origin = rig_core::message::Origin::new("remote.api", "remote-mock", "remote");
        let response = rig_core::completion::CompletionResponse::new(
            vec![rig_core::message::AssistantContent::text("REMOTE SERVED")],
            rig_core::completion::Usage::default(),
            origin,
            json!({}),
        );
        rig_core::serve::Reply::Outcome(Ok(rig_core::effect::Outcome::Completion(response)))
    }
}

/// 回归（§29.2 保留项）：已登记 Remote + LocalModelOnly → remote 永不选中。
#[test]
fn local_model_only_forbids_registered_remote_model_selection() {
    let remote_served = Arc::new(AtomicBool::new(false));
    let model = bevy_needle::rig::needle3_model(
        Arc::new(MockBackend::new(vec![json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": "local answer",
        })])),
        "needle3",
    );
    let remote_flag = remote_served.clone();
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    bevy_needle::rig::install_security_guard(&mut app);
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = model.clone();
        let remote_flag = remote_flag.clone();
        commands.queue(move |world: &mut World| {
            let local_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                None,
            )
            .expect("register local");
            let remote_entity = bevy_needle::rig::register_remote_model(
                world,
                REMOTE_MODEL_KEY,
                RemoteMock { served: remote_flag },
            )
            .expect("register remote");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("sec".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(64)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(1)),
                    rig_ecs::agent::MaxTurns(1),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(local_entity),
                ))
                .id();
            let run_a = world.spawn_run(agent, &[], "本地问题", false, None);
            let run_b = world.spawn_run(agent, &[], "远端问题", false, None);
            world
                .entity_mut(run_b)
                .insert(rig_ecs::agent::UsesModel(remote_entity));
            let _ = run_a;
        });
    });

    drive_to_two_endings(&mut app);

    let world = app.world_mut();
    let mut settled = world.query::<(&RunResult, &Settled)>();
    let answers: Vec<String> = settled.iter(world).map(|(r, _)| r.0.clone()).collect();
    assert_eq!(
        answers,
        vec!["local answer".to_string()],
        "本地 run 必须正常 settle（护栏不误伤）：{answers:?}"
    );
    let mut failed = world.query::<&Failed>();
    let failure = failed.iter(world).next().expect("remote run must be failed");
    let rig_ecs::agent::Failure::Cancelled(report) = &failure.0 else {
        panic!("护栏拒绝应是 Cancelled 失败：{:?}", failure.0);
    };
    assert!(
        report.to_string().contains("LocalModelOnly"),
        "拒绝原因应来自安全护栏：{report}"
    );
    assert!(
        !remote_served.load(Ordering::SeqCst),
        "LocalModelOnly 下远端模型绝不能被 served"
    );
    let guard = world.resource::<bevy_needle::rig::SecurityGuard>();
    assert_eq!(guard.blocked, 1, "护栏拦截计数");
}

/// §29.2 新增 E2E：**未分类** handler + LocalModelOnly ⇒ dispatch denied。
///
/// fail-closed 的核心语义：宿主绕过本 crate 助手（直接 `Handlers::register`）
/// 注册的模型**没有分类**；LocalModelOnly 下默认视为 remote——不存在靠
/// "忘了登记"而获得放行的缺口（v28 审计 A 的 fail-open 修正）。
#[test]
fn local_model_only_denies_unclassified_handlers_by_default() {
    let remote_served = Arc::new(AtomicBool::new(false));
    let remote_flag = remote_served.clone();
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    bevy_needle::rig::install_security_guard(&mut app);
    app.add_systems(Startup, move |mut handlers: Handlers, mut commands: Commands| {
        let remote_flag = remote_flag.clone();
        // 裸注册（不经本 crate 助手）：**无分类**——fail-closed 的靶子。
        let remote_entity = handlers
            .register(
                REMOTE_MODEL_KEY,
                RemoteMock { served: remote_flag },
            )
            .expect("raw register");
        commands.queue(move |world: &mut World| {
            // Run：显式选中未分类 handler。
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("bypass".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(64)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(1)),
                    rig_ecs::agent::MaxTurns(1),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(remote_entity),
                ))
                .id();
            world.spawn_run(agent, &[], "绕过分类的问题", false, None);
        });
    });

    drive_to_terminal(&mut app, 300);

    let world = app.world_mut();
    let mut failed = world.query::<&Failed>();
    let failure = failed.iter(world).next().expect("unclassified run must fail");
    let rig_ecs::agent::Failure::Cancelled(report) = &failure.0 else {
        panic!("fail-closed 拒绝应是 Cancelled 失败：{:?}", failure.0);
    };
    assert!(
        report.to_string().contains("LocalModelOnly"),
        "拒绝原因应来自安全护栏：{report}"
    );
    assert!(
        !remote_served.load(Ordering::SeqCst),
        "未分类 handler 在 LocalModelOnly 下绝不能被 served"
    );
    let guard = world.resource::<bevy_needle::rig::SecurityGuard>();
    assert_eq!(guard.blocked, 1, "护栏拦截计数");
}

// ─────────────────── ⑥ cancellation（任务 E）───────────────────

/// 阻塞型后端：complete() 挂起直到测试放行。
struct HostGateBackend {
    release_rx: Mutex<std::sync::mpsc::Receiver<()>>,
}

impl bevy_needle::backend::NeedleBackend for HostGateBackend {
    fn bind(
        &self,
        _signature: u64,
        _system: &str,
        _tools_json: &str,
        _tool_index: Option<&std::path::Path>,
    ) -> Result<(), bevy_needle::error::NeedleError> {
        Ok(())
    }

    fn complete(
        &self,
        _input: &str,
        _max_new_tokens: u32,
        _buffer: &mut [u8],
    ) -> Result<bevy_needle::engine::NeedleResponse, bevy_needle::error::NeedleError> {
        let _ = self.release_rx.lock().expect("gate").recv();
        let body = serde_json::to_vec(&json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": "late complete after cancel",
        }))
        .expect("serialize");
        bevy_needle::engine::NeedleResponse::parse(&body)
    }

    fn reset(&self) {}
}

/// 取消测试的 run 标记组件（spawn_run 后回查 run 实体用）。
#[derive(Component)]
struct CancelFlag(bevy_ecs::entity::Entity);

/// ⑥：在途取消 → Failed(Cancelled)；迟到完成不复活 run。
#[test]
fn cancel_run_mid_flight_discards_late_completion() {
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let model = bevy_needle::rig::needle3_model(
        Arc::new(HostGateBackend {
            release_rx: Mutex::new(release_rx),
        }),
        "needle3",
    );
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    bevy_needle::rig::install_security_guard(&mut app); // 统一带着护栏跑
    app.add_systems(Startup, move |mut commands: Commands| {
        let queued_model = model.clone();
        commands.queue(move |world: &mut World| {
            let model_entity = bevy_needle::rig::register_local_model(
                world,
                "needle3",
                queued_model,
                None,
            )
            .expect("register");
            let agent = world
                .spawn((
                    rig_ecs::agent::Owner("cancel".to_owned()),
                    rig_ecs::agent::Preamble(None),
                    rig_ecs::agent::Temperature(None),
                    rig_ecs::agent::MaxTokens(Some(64)),
                    rig_ecs::agent::AdditionalParams(None),
                    rig_ecs::agent::ToolChoiceSpec(None),
                    rig_ecs::agent::Output::default(),
                    rig_ecs::agent::DefaultMaxTurns(Some(2)),
                    rig_ecs::agent::MaxTurns(2),
                    rig_ecs::agent::InvalidCalls::default(),
                    rig_ecs::agent::UsesModel(model_entity),
                ))
                .id();
            let run = world.spawn_run(agent, &[], "会被取消的问题", false, None);
            world.entity_mut(run).insert(CancelFlag(run));
        });
    });

    // 1. 等到完成 effect 真正在途（InFlight）。
    let mut run = None;
    for _ in 0..400 {
        app.update();
        let world = app.world_mut();
        let mut issued = world.query::<(&rig_ecs::bus::InFlight, &rig_ecs::bus::PendingEffect)>();
        if issued.iter(world).next().is_some() {
            let mut marker = world.query::<&CancelFlag>();
            run = marker.iter(world).next().map(|f| f.0);
            if run.is_some() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let run = run.expect("effect must be in flight");

    // 2. 取消（rig-ecs 原生 cancel_run）。
    {
        let world = app.world_mut();
        world.cancel_run(run, "user cancelled");
    }
    // 3. 终态到达：Failed(Cancelled)。
    let mut attempts = 0;
    loop {
        app.update();
        let world = app.world_mut();
        let mut failed = world.query::<&Failed>();
        if let Some(failure) = failed.iter(world).next() {
            let rig_ecs::agent::Failure::Cancelled(report) = &failure.0 else {
                panic!("取消应落 Cancelled 失败：{:?}", failure.0);
            };
            assert!(report.to_string().contains("user cancelled"));
            break;
        }
        attempts += 1;
        assert!(attempts < 400, "run did not fail after cancel");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    // 4. 放行 worker → 迟到完成落回（不复活 run）。
    let _ = release_tx.send(());
    for _ in 0..20 {
        app.update();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let world = app.world_mut();
    let mut status = world.query::<&Failed>();
    let failure = status.iter(world).next().expect("run keeps its ending");
    assert!(
        matches!(failure.0, rig_ecs::agent::Failure::Cancelled(_)),
        "迟到结果不得复活 run：{:?}",
        failure.0
    );
    let mut results = world.query::<&RunResult>();
    assert!(
        results.iter(world).next().is_none(),
        "取消后不得提交答案到历史"
    );
}
