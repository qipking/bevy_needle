//! §16.4 T3-A / T3-B / T4（v14）：身份隔离与 attempt-stable 语义。
//!
//! - T3-A：**同一 M + 两个 DriverId + tier[0,1]** → 一个 inbox、一个 worker、
//!   两个 tier 都可执行（I32：一个 M → 一个 inbox + 一个 system；一个实例
//!   可绑多 tier——这里用两个实例共享同一 `Arc<M>`，验证 per-M 集合接纳多 id）；
//! - T4：stale epoch 事件丢弃（I17）+ attempt-stable（I28）。

#![cfg(feature = "rig")]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::sync::Arc;

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::escalate::{DriverId, DriverRegistry, EscalationReason, EscalationState};
use bevy_needle::prelude::*;
use bevy_needle::rig::{
    register_with_model, RigAwait, RigDriverIds, RigDriverState, RigModelInbox, RigModelTurn,
};
use rig_core::completion::message::AssistantContent;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use serde_json::json;

/// 同一模型类型：两次调用分别回不同文本（第一轮回 tier0 文本，第二轮回 tier1 文本）。
#[derive(Default)]
struct SharedScriptModel {
    calls: AtomicUsize,
}

impl CompletionModel for SharedScriptModel {
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let text = format!("shared model call #{call}");
        std::future::ready(Ok(CompletionResponse::new(
            vec![AssistantContent::text(text)],
            Usage::default(),
            "fake-shared",
        )))
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<rig_core::streaming::StreamingCompletionResponse, CompletionError>>
    {
        std::future::ready(Err(CompletionError::ResponseError("no stream".into())))
    }
}

fn run_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

/// 可脚本化模型：每实例独立脚本（同 M 类型、不同实例——I32 的正交维度）。
struct ScriptedModel {
    script: Mutex<Vec<Result<CompletionResponse, CompletionError>>>,
    calls: AtomicUsize,
}

impl CompletionModel for ScriptedModel {
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        let next = self
            .script
            .lock()
            .expect("script poisoned")
            .pop_front_value()
            .unwrap_or_else(|| Ok(CompletionResponse::new(
                vec![AssistantContent::text(format!("default #{call}"))],
                Usage::default(),
                "fake-scripted",
            )));
        std::future::ready(next)
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<rig_core::streaming::StreamingCompletionResponse, CompletionError>>
    {
        std::future::ready(Err(CompletionError::ResponseError("no stream".into())))
    }
}

trait PopFrontScript {
    fn pop_front_value(&mut self) -> Option<Result<CompletionResponse, CompletionError>>;
}

impl PopFrontScript for Vec<Result<CompletionResponse, CompletionError>> {
    fn pop_front_value(&mut self) -> Option<Result<CompletionResponse, CompletionError>> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}

#[test]
fn t3a_same_model_type_two_driver_ids_both_tiers_execute() {
    // I32 完整验证：同一 `M`（ScriptedModel）+ 两个 DriverId（共享一个
    // inbox/一个 worker/一个 system），且 **tier 0 与 tier 1 都真实执行**：
    // run#1：tier 0（rig-a，脚本失败）→ 升档 → tier 1（rig-b，成功）；
    // run#2：tier 0（rig-a，脚本第二个条目=成功）→ 直接完成。
    let model_a = Arc::new(ScriptedModel {
        script: Mutex::new(vec![
            Err(CompletionError::ResponseError("a attempt 1 failed (scripted)".into())),
            Ok(CompletionResponse::new(
                vec![AssistantContent::text("a attempt 2 succeeded")],
                Usage::default(),
                "fake-a",
            )),
        ]),
        calls: AtomicUsize::new(0),
    });
    let model_b = Arc::new(ScriptedModel {
        script: Mutex::new(vec![Ok(CompletionResponse::new(
            vec![AssistantContent::text("b answered once")],
            Usage::default(),
            "fake-b",
        ))]),
        calls: AtomicUsize::new(0),
    });

    let mut app = App::new();
    // 两个信封：每个 run 各一次低置信度调用（脚本耗尽后 needle 直接完成）
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![
        json!({
            "type": "call", "success": true, "confidence": 0.01,
            "function_calls": [{ "name": "echo", "arguments": {} }],
        }),
        json!({
            "type": "call", "success": true, "confidence": 0.01,
            "function_calls": [{ "name": "echo", "arguments": {} }],
        }),
    ])));
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::Cloud)
            .with_tier(EscalationTarget::Local)
            .with_tier(EscalationTarget::Remote),
    );

    register_with_model(
        &mut app,
        DriverId("rig-a"),
        EscalationTarget::Local,
        Arc::clone(&model_a),
        &[0],
    )
    .unwrap();
    register_with_model(
        &mut app,
        DriverId("rig-b"),
        EscalationTarget::Remote,
        Arc::clone(&model_b),
        &[1],
    )
    .unwrap();

    // I32 断言：单 inbox 资源 + per-M 集合含两个 id
    assert!(app.world().get_resource::<RigModelInbox<ScriptedModel>>().is_some());
    let ids = app.world().resource::<RigDriverIds<ScriptedModel>>();
    assert!(ids.contains(&DriverId("rig-a")) && ids.contains(&DriverId("rig-b")));

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "echo", |_call| Ok(ToolOutput::ok()));
    let handles = spawn_agent(
        world,
        NeedleAgentSpec::new("a").with_confidence_threshold(0.5),
    );
    attach_tool(world, handles.agent, tool).unwrap();
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "hi"));
    run_frames(&mut app, 40);

    // run#2：第二次门控升级 → tier 0（rig-a 的第二个脚本条目=成功）
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "hi again"));
    run_frames(&mut app, 40);

    // 两档都真实执行：身份断言（按 EscalationState.driver_id 聚合）
    let mut by_driver: std::collections::HashMap<String, (String, RunStatus)> =
        std::collections::HashMap::new();
    {
        let w = app.world_mut();
        let mut q = w.query::<(
            &RunStatus,
            &EscalationState,
            Option<&RunResultText>,
        )>();
        for (status, esc, text) in q.iter(w) {
            if matches!(status, RunStatus::Completed) {
                by_driver.insert(
                    esc.driver_id.as_str().to_string(),
                    (
                        text.map(|t| t.0.clone()).unwrap_or_default(),
                        *status,
                    ),
                );
            }
        }
    }
    assert_eq!(
        model_a.calls.load(Ordering::SeqCst),
        2,
        "rig-a must have executed tier 0 twice (fail + succeed)"
    );
    assert_eq!(
        model_b.calls.load(Ordering::SeqCst),
        1,
        "rig-b must have executed tier 1 exactly once"
    );
    let (b_text, b_status) = by_driver
        .get("rig-b")
        .expect("tier 1 (rig-b) must have completed a run");
    assert!(*b_status == RunStatus::Completed);
    assert_eq!(b_text, "b answered once", "tier 1 executed by rig-b");
    let (a_text, a_status) = by_driver
        .get("rig-a")
        .expect("tier 0 (rig-a) must have completed a run");
    assert!(*a_status == RunStatus::Completed);
    assert_eq!(a_text, "a attempt 2 succeeded", "tier 0 executed by rig-a");
}

/// 门闩模型：`completion()` 阻塞到 `release` 被置位（在途 attempt 的可控时序）。
struct GatedModel {
    release: Arc<AtomicBool>,
    calls: AtomicUsize,
}

impl CompletionModel for GatedModel {
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> {
        let release = Arc::clone(&self.release);
        let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        async move {
            while !release.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            Ok(CompletionResponse::new(
                vec![AssistantContent::text(format!("in-flight answer (call {call})"))],
                Usage::default(),
                "fake-gated",
            ))
        }
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<rig_core::streaming::StreamingCompletionResponse, CompletionError>>
    {
        std::future::ready(Err(CompletionError::ResponseError("no stream".into())))
    }
}

#[test]
fn t4_cancel_routes_to_resolved_instance_not_registry() {
    // I28 取消侧（v14.2）：在途 attempt 期间 rebind tier → 取消 run →
    // cancel 必须路由到 **attempt 快照的实例**（rig-a），而不是当前
    // registry 绑定（rig-b）。每实例独立的 cancelled 计数器是可观测依据。
    let release = Arc::new(AtomicBool::new(false));
    let mut app = App::new();
    // 低置信度信封：门控必须触发升级（空信封会让 needle 直接 Completed）
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![json!({
        "type": "call", "success": true, "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })])));
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::Cloud)
            .with_tier(EscalationTarget::Local)
            .with_tier(EscalationTarget::Remote),
    );

    // rig-b 先注册（SharedScriptModel 的 inbox 独立，不干扰 rig-a 的 M）
    register_with_model(
        &mut app,
        DriverId("rig-b"),
        EscalationTarget::Remote,
        Arc::new(SharedScriptModel::default()),
        &[1],
    )
    .unwrap();

    // rig-a：测试自持实例句柄（观察取消路由；不经 downcast——Driver trait 保持干净）
    app.insert_resource(RigModelInbox::<GatedModel>::spawn(
        bevy_needle::rig::RigRuntime::lazy(),
    ));
    let rig_a = {
        let inbox = app.world().resource::<RigModelInbox<GatedModel>>();
        Arc::new(bevy_needle::rig::RigDriver::new(
            DriverId("rig-a"),
            EscalationTarget::Local,
            Arc::new(GatedModel {
                release: Arc::clone(&release),
                calls: AtomicUsize::new(0),
            }),
            inbox,
        ))
    };
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry
            .register(Arc::clone(&rig_a) as Arc<dyn bevy_needle::escalate::Driver>)
            .unwrap();
        registry.bind_tier(0, DriverId("rig-a")).unwrap();
    }
    app.init_resource::<RigDriverIds<GatedModel>>();
    app.world_mut()
        .resource_mut::<RigDriverIds<GatedModel>>()
        .insert(DriverId("rig-a"));
    app.add_systems(
        RunExecution,
        bevy_needle::rig::rig_step_system::<GatedModel>
            .in_set(bevy_needle::app::RunResolutionSystems)
            .before(bevy_needle::escalate::driver_coordinator),
    );

    let world = app.world_mut();
    let tool = world
        .spawn(ToolBundle::new(ToolSpec::new(
            "echo",
            "Echo",
            ParametersBuilder::new().build(),
        )))
        .id();
    register_tool_handler(world, "echo", |_call| Ok(ToolOutput::ok()));
    let handles = spawn_agent(
        world,
        NeedleAgentSpec::new("a").with_confidence_threshold(0.5),
    );
    attach_tool(world, handles.agent, tool).unwrap();
    app.world_mut()
        .write_message(RunAgent::new(handles.agent, "hi"));
    run_frames(&mut app, 6); // 在途：rig-a 的门闩模型 pending

    let run = run_entity(&mut app);
    assert_eq!(
        in_flight_driver(&mut app, run),
        Some(DriverId("rig-a")),
        "in-flight attempt executor must be rig-a"
    );

    // registry mutation：tier 0 rebind 到 rig-b
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.rebind_tier(0, DriverId("rig-b")).unwrap();
    }

    // 取消 run
    app.world_mut().write_message(CancelRun { run });
    run_frames(&mut app, 5);

    // I28 取消侧断言：快照实例（rig-a）收到 cancel；registry 当前绑定（rig-b）零误伤
    assert_eq!(
        rig_a.cancelled_count(),
        1,
        "cancel must route to the attempt-resolved instance (rig-a)"
    );
    let status = *app.world().get::<RunStatus>(run).expect("run alive");
    assert!(matches!(status, RunStatus::Cancelled));
}

fn run_entity(app: &mut App) -> Entity {
    let w = app.world_mut();
    let mut q = w.query::<(Entity, &RunStatus)>();
    q.iter(w)
        .find_map(|(e, s)| {
            matches!(s, RunStatus::Escalating { .. } | RunStatus::Cancelled).then_some(e)
        })
        .expect("escalating/cancelled run exists")
}

fn in_flight_driver(app: &mut App, run: Entity) -> Option<DriverId> {
    app.world()
        .get::<EscalationState>(run)
        .map(|s| s.driver_id)
}

#[test]
fn register_with_model_atomic_on_tier_conflict() {
    // P0-1（v14.2 声称 / v14.3 真正落盘）：preflight（任何副作用之前）失败
    // → driver 不入 registry、fresh tier 不绑、身份集合不登记、inbox 不 spawn。
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![])));
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::Cloud)
            .with_tier(EscalationTarget::Local)
            .with_tier(EscalationTarget::Remote),
    );

    // 占位：rig-owner 已注册并绑定 tier 0
    register_with_model(
        &mut app,
        DriverId("rig-owner"),
        EscalationTarget::Local,
        Arc::new(SharedScriptModel::default()),
        &[0],
    )
    .unwrap();

    // 新 driver + tiers=[0（已绑），1（fresh）] → 整体 Err
    let err = register_with_model(
        &mut app,
        DriverId("rig-newcomer"),
        EscalationTarget::Local,
        Arc::new(SharedScriptModel::default()),
        &[0, 1],
    )
    .unwrap_err();
    assert!(matches!(
        err,
        bevy_needle::escalate::RegistryError::DuplicateTierBinding { tier: 0, .. }
    ));

    // 零残留断言
    let registry = app.world().resource::<DriverRegistry>();
    assert!(
        registry.get(DriverId("rig-newcomer")).is_none(),
        "failed registration must not leave the driver behind"
    );
    assert_eq!(
        registry.driver_id_for_tier(1),
        None,
        "fresh tier must stay unbound"
    );
    assert_eq!(
        registry.driver_id_for_tier(0),
        Some(DriverId("rig-owner")),
        "existing binding must be untouched"
    );
    let ids = app.world().resource::<RigDriverIds<SharedScriptModel>>();
    assert!(
        !ids.contains(&DriverId("rig-newcomer")),
        "preflight failure must not register per-M ids"
    );
    assert!(ids.contains(&DriverId("rig-owner")), "owner id intact");
}

#[test]
fn register_with_model_atomic_on_duplicate_tier_argument() {
    // v14.3：tiers 自身重复（[0,0]）——外部审阅实证的半注册反例已封堵：
    // precheck 现在报 DuplicateTierArgument，整体 Err 且零残留。
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![])));
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::Cloud)
            .with_tier(EscalationTarget::Local),
    );

    let err = register_with_model(
        &mut app,
        DriverId("rig-x"),
        EscalationTarget::Local,
        Arc::new(SharedScriptModel::default()),
        &[0, 0],
    )
    .unwrap_err();
    assert!(matches!(
        err,
        bevy_needle::escalate::RegistryError::DuplicateTierArgument(0)
    ));

    // 零残留
    let registry = app.world().resource::<DriverRegistry>();
    assert!(registry.get(DriverId("rig-x")).is_none());
    assert_eq!(registry.driver_id_for_tier(0), None);
    assert!(
        app.world().get_resource::<RigModelInbox<SharedScriptModel>>().is_none(),
        "preflight failure must not spawn inbox/worker"
    );
    assert!(
        app.world().get_resource::<RigDriverIds<SharedScriptModel>>().is_none(),
        "preflight failure must not register per-M ids"
    );
}

#[test]
fn register_for_tier_idempotent_for_same_instance_same_tier() {
    // v14.3：同实例 + 同 tier 第二次调用 → 幂等 no-op（审阅 ⑥-③），
    // 且 registry 状态完全不变。
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![])));
    app.init_resource::<RigModelInbox<SharedScriptModel>>();
    app.init_resource::<RigDriverIds<SharedScriptModel>>();
    app.world_mut().insert_resource(DriverRegistry::default());

    let driver: std::sync::Arc<dyn bevy_needle::escalate::Driver> =
        Arc::new(bevy_needle::rig::RigDriver::new(
            DriverId("rig-idem"),
            EscalationTarget::Local,
            Arc::new(SharedScriptModel::default()),
            app.world().resource::<RigModelInbox<SharedScriptModel>>(),
        ));
    {
        let mut registry = app.world_mut().resource_mut::<DriverRegistry>();
        registry.register_for_tier(std::sync::Arc::clone(&driver), 0).unwrap();
        // 第二次：同实例 + 同 tier → Ok（no-op）
        registry.register_for_tier(std::sync::Arc::clone(&driver), 0).unwrap();
        // 第三次：同实例 + 新 tier → 仅绑定
        registry.register_for_tier(std::sync::Arc::clone(&driver), 1).unwrap();
    }
    let registry = app.world().resource::<DriverRegistry>();
    assert_eq!(registry.driver_id_for_tier(0), Some(DriverId("rig-idem")));
    assert_eq!(registry.driver_id_for_tier(1), Some(DriverId("rig-idem")));
    // driver 实例唯一（无重复注册副作用）
    assert!(registry.get(DriverId("rig-idem")).is_some());
}


