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
    let model = needle3_model(
        std::sync::Arc::new(MockBackend::new(vec![
            json!({
                "type": "respond",
                "success": true,
                "function_calls": [],
                "reasoning": "first answer",
            }),
            json!({
                "type": "respond",
                "success": true,
                "function_calls": [],
                "reasoning": "second answer",
            }),
        ])),
        "needle3",
    );
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
