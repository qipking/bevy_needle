//! G2 验收（升级计划 §17.2）：rig-ecs host e2e——无网络。
//!
//! ```text
//! User prompt
//!   ↓ rig-ecs spawn_run（原生 Run/Turn/Effect）
//! CompletionRequest（fold_request，含工具面）
//!   ↓ Needle3Model（Needle worker 解码）
//! function_calls
//!   ↓ rig-ecs Materialise → PendingEffect(ToolCall) → ToolFn handler
//! ToolOutput（bevy_needle 纯函数 handler 执行）
//!   ↓ rig-ecs tool batch 回喂（一 user utterance，call order）
//! CompletionRequest #2
//!   ↓ Needle3Model
//! respond → RunResult（Settled）
//! ```
//!
//! `MockBackend` 脚本化两轮信封（call → respond）证明完整闭环。

#![cfg(feature = "rig-ecs")]

use bevy_app::{App, Startup};
use bevy_ecs::prelude::*;
use rig_ecs::bus::Handlers;
use std::sync::Arc;
use std::sync::Mutex;
use bevy_needle::rig::{needle3_model, Needle3Model};
use bevy_needle::MockBackend;
use rig_core::completion::message::Message;
use rig_ecs::prelude::*;
use rig_ecs::systems::RunCommands;
use serde_json::{json, Value};

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

fn app_with(model: Needle3Model) -> App {
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    // 注册模型与工具（G2：不重复注册 bevy_needle 工具表）。
    // model 装进 ResMut 槽，避免闭包 move（FnMut 约束）。
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(Startup, |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
        let Some(model) = setup.0.take() else { return };
        setup_model(&mut handlers, &mut commands, model);
    });
    app
}

#[derive(Resource)]
struct SetupModel(Option<Needle3Model>);

fn setup_model(handlers: &mut Handlers, commands: &mut Commands, model: Needle3Model) {
    let model_entity = handlers
            .register(
                bevy_needle::rig::needle_model_key("needle3"),
                rig_core::serve::adapters::ModelAdapter::new(
                    bevy_needle::rig::NEEDLE_LABEL,
                    rig_core::driver::DynModel::from(model.into_inner()),
                ),
            )
            .expect("register needle model");
        // 一个带纯函数 handler 的工具（ToolCall → ToolOutput）。
        let tool = handlers
            .register(
                bevy_needle::rig::tool_key("set_volume"),
                rig_core::serve::adapters::ToolFn::new(
                    "set_volume",
                    "Set the playback volume.",
                    json!({"type":"object","properties":{"percent":{"type":"integer"}},"required":["percent"]}),
                    move |_context: &mut rig_core::tool::ToolContext, args: Value| {
                        let percent = args.get("percent").and_then(Value::as_i64).unwrap_or(0);
                        Box::pin(async move {
                            Ok(rig_core::tool::ToolOutput::json(json!({ "volume_set": percent })))
                        })
                    },
                ),
            )
            .expect("register tool");
        let agent = commands
            .spawn((
                rig_ecs::agent::Owner("editor".to_owned()),
                rig_ecs::agent::Preamble(Some("You are an audio editor assistant.".into())),
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
    commands.spawn((rig_ecs::agent::Grant(tool), ChildOf(agent)));
    commands.queue(move |world: &mut World| {
        world.spawn_run(agent, &[], "把音量调到 80", false, None);
    });
}

#[test]
fn rig_ecs_agent_runs_two_turn_tool_loop() {
    let model = needle3_model(
        std::sync::Arc::new(two_turn_backend()),
        "needle3",
    );
    let mut app = app_with(model);
    // 驱动到 settle（上限保护）。
    let settled = {
        let mut attempts = 0;
        loop {
            app.update();
            let mut query = app.world_mut().query::<(&RunResult, &Settled)>();
            if let Some((result, _)) = query.iter(app.world()).next() {
                break result.0.clone();
            }
            let mut failed = app.world_mut().query::<&Failed>();
            if let Some(failure) = failed.iter(app.world()).next() {
                panic!("run failed: {:?}", failure.0);
            }
            attempts += 1;
            assert!(attempts < 200, "run did not settle in time");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    };
    assert_eq!(settled, "volume is now 80");
}

#[test]
fn needle_model_serves_completion_effect_directly() {
    // bus 直连形态：PendingEffect(Completion) → Needle3Model → EffectOutcome。
    let model = needle3_model(
        std::sync::Arc::new(MockBackend::new(vec![json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": "hello from needle",
        })])),
        "needle3",
    );
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(Startup, |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
        let Some(model) = setup.0.take() else { return };
        handlers
            .register(
                "model:needle3-direct",
                rig_core::serve::adapters::ModelAdapter::new(
                    bevy_needle::rig::NEEDLE_LABEL,
                    rig_core::driver::DynModel::from(model.into_inner()),
                ),
            )
            .expect("register");
        commands.spawn(rig_ecs::bus::PendingEffect::new(
            "model:needle3-direct",
            rig_core::effect::EffectKind::Completion {
                request: rig_core::completion::CompletionRequest::from(vec![Message::user("hello")]),
                stream: false,
            },
        ));
    });
    let mut attempts = 0;
    loop {
        app.update();
        if attempts % 40 == 0 && attempts > 0 {
            let pending = app
                .world_mut()
                .query::<&rig_ecs::bus::PendingEffect>()
                .iter(app.world())
                .count();
            let issued = app
                .world_mut()
                .query::<&rig_ecs::bus::Issued>()
                .iter(app.world())
                .count();
            let inflight = app
                .world_mut()
                .query::<&rig_ecs::bus::InFlight>()
                .iter(app.world())
                .count();
            eprintln!("attempt {attempts}: pending={pending} issued={issued} inflight={inflight}");
        }
        let mut query = app.world_mut().query::<&rig_ecs::bus::EffectOutcome>();
        if let Some(outcome) = query.iter(app.world()).next() {
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


/// G6（升级计划 §17.6）：同一模型实例、两个并发 Run——history 与执行不串。
///
/// rig-ecs 的 run 是独立实体（每个 run 自己的 utterance 图），Needle3Model
/// 共享同一 worker（单会话串行）；本测试断言两个 run 各自拿到自己的答案
/// 且互不串话（mock 脚本按 dispatch 顺序回放）。
#[test]
fn two_runs_same_model_do_not_cross_talk() {
    // I33 / §27.6：G6 验收升级为 **payload 级断言**——捕获引擎实际收到的
    // 每一份输入，断言没有一次请求混入另一个 Run 的 utterance。
    let engine_inputs: std::sync::Arc<std::sync::Mutex<Vec<String>>> =
        std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = engine_inputs.clone();
    let backend = MockBackend::dynamic(move |input: &str| {
        recorder.lock().expect("engine input recorder").push(input.to_owned());
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
    let model = needle3_model(std::sync::Arc::new(backend), "needle3");
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(
        Startup,
        |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
            let Some(model) = setup.0.take() else { return };
            let model_entity = handlers
                .register(
                    bevy_needle::rig::needle_model_key("needle3"),
                    rig_core::serve::adapters::ModelAdapter::new(
                        bevy_needle::rig::NEEDLE_LABEL,
                        rig_core::driver::DynModel::from(model.into_inner()),
                    ),
                )
                .expect("register");
            // 同一 agent：两次 spawn_run（共享模型与工具面，串行调度）。
            let agent = commands
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
            commands.queue(move |world: &mut World| {
                world.spawn_run(agent, &[], "first", false, None);
                world.spawn_run(agent, &[], "second", false, None);
            });
        },
    );

    let mut answers = Vec::new();
    let mut attempts = 0;
    while answers.len() < 2 {
        app.update();
        let mut query = app.world_mut().query::<(&RunResult, &Settled)>();
        for (result, _) in query.iter(app.world()) {
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

/// G7（升级计划 §17.7）：重复循环稳定性——同一模型连续 N 次单轮 run。
///
/// 验收：无 permanent Escalating（本路径无 escalate）、无 lost event、
/// 无 duplicate execution（mock 脚本按序耗尽）。
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
    let model = needle3_model(std::sync::Arc::new(MockBackend::new(script)), "needle3");
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(
        Startup,
        |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
            let Some(model) = setup.0.take() else { return };
            let model_entity = handlers
                .register(
                    bevy_needle::rig::needle_model_key("needle3"),
                    rig_core::serve::adapters::ModelAdapter::new(
                        bevy_needle::rig::NEEDLE_LABEL,
                        rig_core::driver::DynModel::from(model.into_inner()),
                    ),
                )
                .expect("register");
            let agent = commands
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
            commands.queue(move |world: &mut World| {
                for round in 0..ROUNDS {
                    world.spawn_run(agent, &[], format!("round-{round}"), false, None);
                }
            });
        },
    );

    let mut settled = Vec::new();
    let mut attempts = 0;
    while settled.len() < ROUNDS {
        app.update();
        let mut query = app.world_mut().query::<(&RunResult, &Settled)>();
        for (result, _) in query.iter(app.world()) {
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


/// 最小 app：注册带 gate 的 Needle 模型 + 单 agent（无工具面），发一轮 run。
fn app_with_direct(model: Needle3Model, threshold: f64) -> App {
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(
        Startup,
        move |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
            let Some(model) = setup.0.take() else { return };
            let model_entity = bevy_needle::rig::register_needle_model(
                &mut handlers,
                "needle3",
                model,
                Some(bevy_needle::rig::ConfidenceGate::new(threshold)),
            )
            .expect("register");
            let agent = commands
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
            commands.queue(move |world: &mut World| {
                world.spawn_run(agent, &[], "说一句", false, None);
            });
        },
    );
    app
}

// ─────────────────────────── ④ confidence gate → rig-ecs 执行路径 ─────────────
//
// §27.4 验收：必须是 **effect 计数**，不是 unit 返回。
//   low confidence  → ToolEffect == 0 且 WorldEffect == 0
//   high confidence → ToolEffect == 1（exactly）
//   final response  → 不得被 gate 阻塞（respond 轮不参与）
//
// 计数方式：`On<Add, PendingEffect>` 观察者按 kind 家族计数（抗 despawn）。

/// ToolCall 家族 effect 的 materialise 计数（27.4 验收的观测面）。
#[derive(Resource, Default)]
struct ToolEffectCount(u64);

/// World-effect 落到 effect 实体上的计数（27.4 的 WorldEffect == 0 断言）。
#[derive(Resource, Default)]
struct WorldQuestionCount(u64);

/// 为具体 WorldEffect `E` 注册 Asked 计数观察者（27.4 WorldEffect == 0）。
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

/// B2 轨的哨兵 WorldEffect（27.4 断言 `WorldEffect == 0` 的计数对象）。
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

/// ④ low confidence：模型产出 tool call 但 gate 拒绝 → **零 tool effect**、
/// **零 world effect**、工具回调零执行，且 run 以门控原因失败（不 settle）。
#[test]
fn confidence_gate_lifts_zero_tool_effects_on_low_confidence() {
    let callback_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    // 两轮脚本：第一轮低置信度 tool call（被拒）——一次信封即可证明
    // "低置信度下绝不 materialise"。
    let model = needle3_model(
        std::sync::Arc::new(MockBackend::new(vec![json!({
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
    app.insert_resource(SetupModel(Some(model)));
    let count_for_closure = callback_count.clone();
    app.add_systems(
        Startup,
        move |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
            let Some(model) = setup.0.take() else { return };
            // 带 gate 注册（阈值 0.85）——本测试执行路径上唯一的 gate
            // （§27.4：bevy_needle 不保留第二个 rig 路径 gate）。
            let model_entity = bevy_needle::rig::register_needle_model(
                &mut handlers,
                "needle3",
                model,
                Some(bevy_needle::rig::ConfidenceGate::new(0.85)),
            )
            .expect("register needle model");
            // B1 纯工具轨：授权工具（计数回调，绝不被允许执行）。
            // 系统体内 clone：内层 move 闭包消费局部副本，外层保持 FnMut。
            let counter = count_for_closure.clone();
            let tool_entity = bevy_needle::rig::register_tool_fn(
                &mut handlers,
                "set_volume",
                "Set the playback volume.",
                json!({"type":"object","properties":{"percent":{"type":"integer"}}}),
                move |_context: &mut rig_core::tool::ToolContext, _args: Value| {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Box::pin(async move {
                        Ok(rig_core::tool::ToolOutput::json(json!({ "volume_set": 1 })))
                    })
                },
            )
            .expect("register tool");
            // B2 World 工具轨也授权（同样绝不允许被走到的另一条路）。
            let world_tool = bevy_needle::rig::register_world_tool::<WatchQuestions>(
                &mut handlers,
                "watch_questions",
            )
            .expect("register world tool");
            let agent = commands
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
                commands.spawn((
                    rig_ecs::agent::Grant(tool),
                    bevy_ecs::hierarchy::ChildOf(agent),
                ));
            }
            commands.queue(move |world: &mut World| {
                world.spawn_run(agent, &[], "把音量调到 1", false, None);
            });
        },
    );

    drive_to_terminal(&mut app, 300);
    let world = app.world_mut();
    let tool_effects = world.resource::<ToolEffectCount>().0;
    let world_questions = world.resource::<WorldQuestionCount>().0;
    assert_eq!(tool_effects, 0, "低置信度 → ToolEffect == 0（§27.4）");
    assert_eq!(world_questions, 0, "低置信度 → WorldEffect == 0（§27.4）");
    assert_eq!(
        callback_count.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "工具回调零执行"
    );
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

/// ④ high confidence：恰好一个 tool effect（exactly-one 断言）+ 回调恰好一次。
#[test]
fn confidence_gate_passes_exactly_one_tool_effect() {
    let callback_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let count_for_closure = callback_count.clone();
    let model = needle3_model(
        std::sync::Arc::new(MockBackend::new(vec![
            json!({
                "type": "call",
                "success": true,
                "function_calls": [{ "name": "set_volume", "arguments": { "percent": 80 } }],
                "confidence": 0.94,
            }),
            json!({
                "type": "respond",
                "success": true,
                "function_calls": [],
                "reasoning": "volume is 80",
                "confidence": 0.9,
            }),
        ])),
        "needle3",
    );
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.init_resource::<ToolEffectCount>();
    app.add_observer(watch_tool_effects);
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(
        Startup,
        move |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
            let Some(model) = setup.0.take() else { return };
            let model_entity = bevy_needle::rig::register_needle_model(
                &mut handlers,
                "needle3",
                model,
                Some(bevy_needle::rig::ConfidenceGate::new(0.85)),
            )
            .expect("register");
            let tool_entity = bevy_needle::rig::register_tool_fn(
                &mut handlers,
                "set_volume",
                "Set the playback volume.",
                json!({"type":"object","properties":{"percent":{"type":"integer"}}}),
                {
                    // 系统体内 clone：内层 move 闭包消费局部副本，外层保持 FnMut。
                    let counter = count_for_closure.clone();
                    move |_context: &mut rig_core::tool::ToolContext, args: Value| {
                        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let percent =
                            args.get("percent").and_then(Value::as_i64).unwrap_or(0);
                        Box::pin(async move {
                            Ok(rig_core::tool::ToolOutput::json(json!({ "volume_set": percent })))
                        })
                    }
                },
            )
            .expect("register tool");
            let agent = commands
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
            commands.spawn((
                rig_ecs::agent::Grant(tool_entity),
                bevy_ecs::hierarchy::ChildOf(agent),
            ));
            commands.queue(move |world: &mut World| {
                world.spawn_run(agent, &[], "把音量调到 80", false, None);
            });
        },
    );

    let mut settled = None;
    let mut attempts = 0;
    while settled.is_none() {
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<(&RunResult, &Settled)>();
        if let Some((result, _)) = query.iter(world).next() {
            settled = Some(result.0.clone());
        }
        attempts += 1;
        assert!(attempts < 400, "run did not settle in time");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(settled, Some("volume is 80".to_string()));
    assert_eq!(
        app.world().resource::<ToolEffectCount>().0,
        1,
        "高置信度 → ToolEffect == 1（exactly，§27.4）"
    );
    assert_eq!(
        callback_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "工具回调恰好执行一次"
    );
}

/// ④ final response：低置信度的 respond 轮**不得**被 gate 阻塞。
#[test]
fn confidence_gate_never_blocks_final_response() {
    let model = needle3_model(
        std::sync::Arc::new(MockBackend::new(vec![json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": "all done without tools",
            "confidence": 0.01,
        })])),
        "needle3",
    );
    let mut app = app_with_direct(model, 0.85);
    let mut attempts = 0;
    loop {
        app.update();
        let world = app.world_mut();
        let mut query = world.query::<(&RunResult, &Settled)>();
        if let Some((result, _)) = query.iter(world).next() {
            assert_eq!(
                result.0,
                "all done without tools",
                "respond 轮必须原样 settle（confidence=0.01 也不阻）"
            );
            break;
        }
        attempts += 1;
        assert!(attempts < 400, "respond run did not settle in time");
        std::thread::sleep(std::time::Duration::from_millis(5));
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


// ─────────────────── ⑥ LocalModelOnly（security.rs 护栏，rig-ecs 执行路径）─────────
//
// §26.7 验收行："已注册 remote + LocalModelOnly → remote 永不选中"。
// 护栏系统（Select 之后、Assemble 之前）对命中 Remote handler 的 run 写
// rig-ecs 标准停止钩子 Cancelled → Failed(Cancelled)；远端 handler 零 served。

/// 远端哨兵模型：被选中即置 served 旗（断言"永不选中"的观测面）。
struct RemoteMock {
    served: Arc<std::sync::atomic::AtomicBool>,
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
        self.served
            .store(true, std::sync::atomic::Ordering::SeqCst);
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

const REMOTE_MODEL_KEY: &str = "model:remote";

/// 取消测试的 run 标记组件（spawn_run 后回查 run 实体用）。
#[derive(Component)]
struct CancelFlag(bevy_ecs::entity::Entity);

/// ⑥：LocalModelOnly 下，显式 `UsesModel(remote)` 的 run 被 rig-ecs 护栏
/// 拒绝（Failed(Cancelled) + 安全原因），远端 handler 零 served；同帧的
/// 本地 run 正常 settle（护栏不误伤）。
#[test]
fn local_model_only_forbids_remote_model_selection() {
    let remote_served = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let model = needle3_model(
        std::sync::Arc::new(MockBackend::new(vec![json!({
            "type": "respond",
            "success": true,
            "function_calls": [],
            "reasoning": "local answer",
        })])),
        "needle3",
    );
    let remote_served_for_closure = remote_served.clone();
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    bevy_needle::rig::install_security_guard(&mut app);
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(
        Startup,
        move |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
            let Some(model) = setup.0.take() else { return };
            // 本地 Needle 模型（Local 类）。
            let local_entity = bevy_needle::rig::register_needle_model(
                &mut handlers, "needle3", model, None,
            )
            .expect("register local");
            // 远端哨兵（Remote 类）——显式注册（§26.7 语义：remote 可注册）。
            let remote_entity = handlers
                .register(
                    REMOTE_MODEL_KEY,
                    RemoteMock { served: remote_served_for_closure.clone() },
                )
                .expect("register remote");
            // 分类登记进护栏（LocalModelOnly 默认 ⇒ remote 被禁）。
            commands.queue(move |world: &mut World| {
                world.resource_mut::<bevy_needle::rig::SecurityGuard>()
                    .insert_remote(remote_entity);
            });
            let agent = commands
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
            commands.queue(move |world: &mut World| {
                // Run A：本地模型 → 正常 settle（护栏不误伤）。
                let run_a = world.spawn_run(agent, &[], "本地问题", false, None);
                // Run B：显式选中 remote（Select 对已有的 UsesModel 不覆盖）。
                let run_b = world.spawn_run(agent, &[], "远端问题", false, None);
                world.entity_mut(run_b).insert(rig_ecs::agent::UsesModel(remote_entity));
                let _ = run_a;
            });
        },
    );

    // 驱动到两站全终态（Settled 与在场 Failed 都算）。
    let mut attempts = 0;
    loop {
        app.update();
        let world = app.world_mut();
        let mut settled = world.query::<(&RunResult, &Settled)>();
        let mut failed = world.query::<&Failed>();
        let settled_count = settled.iter(world).count();
        let failed_count = failed.iter(world).count();
        if settled_count >= 1 && failed_count >= 1 {
            break;
        }
        attempts += 1;
        assert!(attempts < 400, "runs did not terminate: settled={settled_count} failed={failed_count}");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let world = app.world_mut();
    // Run A：本地正常完成。
    let mut settled = world.query::<(&RunResult, &Settled)>();
    let answers: Vec<String> = settled.iter(world).map(|(r, _)| r.0.clone()).collect();
    assert_eq!(
        answers,
        vec!["local answer".to_string()],
        "本地 run 必须正常 settle（护栏不误伤）：{answers:?}"
    );
    // Run B：护栏拒绝（Failed(Cancelled)，安全原因）。
    let mut failed = world.query::<&Failed>();
    let failure = failed.iter(world).next().expect("remote run must be failed");
    let rig_ecs::agent::Failure::Cancelled(report) = &failure.0 else {
        panic!("护栏拒绝应是 Cancelled 失败：{:?}", failure.0);
    };
    assert!(
        report.to_string().contains("LocalModelOnly"),
        "拒绝原因应来自安全护栏：{report}"
    );
    // 远端 handler 零 served；护栏计数 +1。
    assert!(
        !remote_served.load(std::sync::atomic::Ordering::SeqCst),
        "LocalModelOnly 下远端模型绝不能被 served"
    );
    let guard = world.resource::<bevy_needle::rig::SecurityGuard>();
    assert_eq!(guard.blocked, 1, "护栏拦截计数");
}

// ─────────────────── ⑥ cancellation（rig-ecs 执行路径）───────────────────
//
// 验收（§26.7 Cancellation 行）：在途取消 → 迟到结果不复活 run。
// 取消落在模型调用在途（worker 阻塞中）：Failed(Cancelled) 终态建立后，
// 放行 worker → 迟到完成必须被丢弃，run 不复活、无答案提交。

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
        let _ = self
            .release_rx
            .lock()
            .expect("gate")
            .recv();
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

/// ⑥：在途取消 → Failed(Cancelled)；迟到完成不复活 run。
#[test]
fn cancel_run_mid_flight_discards_late_completion() {
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let model = needle3_model(
        std::sync::Arc::new(HostGateBackend {
            release_rx: Mutex::new(release_rx),
        }),
        "needle3",
    );
    let mut app = App::new();
    app.add_plugins(rig_ecs::RigPlugin::default());
    app.insert_resource(SetupModel(Some(model)));
    app.add_systems(
        Startup,
        move |mut handlers: Handlers, mut commands: Commands, mut setup: ResMut<SetupModel>| {
            let Some(model) = setup.0.take() else { return };
            let model_entity = bevy_needle::rig::register_needle_model(
                &mut handlers, "needle3", model, None,
            )
            .expect("register");
            let agent = commands
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
            commands.queue(move |world: &mut World| {
                let run = world.spawn_run(agent, &[], "会被取消的问题", false, None);
                world.entity_mut(run).insert(CancelFlag(run));
            });
        },
    );

    // 1. 等到完成 effect 真正在途（Issued / InFlight）。
    let run = {
        let mut found = None;
        for _ in 0..400 {
            app.update();
            let world = app.world_mut();
            let mut issued = world.query::<(&rig_ecs::bus::InFlight, &rig_ecs::bus::PendingEffect)>();
            if issued.iter(world).next().is_some() {
                let mut marker = world.query::<&CancelFlag>();
                found = marker.iter(world).next().map(|f| f.0);
                if found.is_some() {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        found.expect("effect must be in flight")
    };

    // 2. 取消（rig-ecs 原生 cancel_run；bevy_needle 不另造取消语义）。
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

    // 4. 放行 worker → 迟到完成落回（effect 归 handler，不复活 run）。
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
