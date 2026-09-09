//! rig 模型接线 e2e（`rig` feature）：**无网络**验证完整模型调用路径。
//!
//! 路径（规格 §13）：CallModel → `CompletionRequest`（worker 侧）
//! → `model.completion(req).await`（**I22：worker 侧唯一 async 点**）
//! → `CompletionResponse` → `ModelTurn` → [`RigModelInbox`] 回灌（**I26：唯一注入点**）
//! → `AgentRun::model_response` → Done → Completed。
//!
//! `FakeModel` 是手工 `impl CompletionModel`（RPITIT 非 dyn-compatible，
//! P1-7 裁决的实证），脚本化响应——生产模型（candle/openai）只需替换它。

#![cfg(feature = "rig")]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use bevy_app::App;
use bevy_ecs::prelude::*;
use bevy_needle::escalate::{DriverRegistry, EscalationReason, EscalationState};
use bevy_needle::prelude::*;
use bevy_needle::escalate::DriverId;
use bevy_needle::rig::{
    register_with_model, RigDriverState, RigModelInbox, RigModelTurn,
};
use rig_core::completion::message::AssistantContent;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, Usage,
};
use serde_json::json;

/// 脚本化模型：按序回放响应（耗尽后回默认文本）；`pending: true` 时永不响应
/// （`std::future::pending`——测试挂起等待语义用）。
#[derive(Default)]
struct FakeModel {
    script: Mutex<Vec<CompletionResponse>>,
    pending: bool,
}

impl FakeModel {
    fn with_text_script(texts: &[&str]) -> Self {
        let script = texts
            .iter()
            .map(|t| CompletionResponse::new(vec![AssistantContent::text(*t)], Usage::default(), "fake"))
            .collect();
        Self {
            script: Mutex::new(script),
            pending: false,
        }
    }

    /// 永不完成（`completion().await` 永远 Pending）。
    fn never() -> Self {
        Self {
            pending: true,
            ..Default::default()
        }
    }
}

impl CompletionModel for FakeModel {
    fn completion(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> {
        async move {
            if self.pending {
                std::future::pending::<()>().await;
            }
            let resp = self
                .script
                .lock()
                .expect("fake model script poisoned")
                .pop_front_value();
            let resp = resp.unwrap_or_else(|| {
                CompletionResponse::new(
                    vec![AssistantContent::text("fake default")],
                    Usage::default(),
                    "fake",
                )
            });
            Ok(resp)
        }
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<rig_core::streaming::StreamingCompletionResponse, CompletionError>>
    {
        std::future::ready(Err(CompletionError::ResponseError(
            "FakeModel does not stream".into(),
        )))
    }
}

trait PopFront {
    fn pop_front_value(&mut self) -> Option<CompletionResponse>;
}

impl PopFront for Vec<CompletionResponse> {
    fn pop_front_value(&mut self) -> Option<CompletionResponse> {
        if self.is_empty() {
            None
        } else {
            Some(self.remove(0))
        }
    }
}

fn run_frames(app: &mut App, frames: usize) {
    for _ in 0..frames {
        app.update();
    }
}

#[test]
fn fake_model_end_to_end_via_worker() {
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![json!({
        "type": "call", "success": true, "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })])));

    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );

    // 接线：启动期预热完成的模型（§13——本测试内联构造即视为"启动期"）
    let model = Arc::new(FakeModel::with_text_script(&["escalated by fake model"]));
    register_with_model(
        &mut app,
        DriverId("rig-fake"),
        EscalationTarget::Local,
        model,
        &[0],
    )
    .expect("rig driver registers");

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
    run_frames(&mut app, 400);

    // 全链路：门控 → coordinator → RigDriver.submit → worker completion().await
    // → inbox 回灌 → Done → Completed
    let completed = {
        let mut q = app
            .world_mut()
            .query::<(&RunStatus, Option<&RunResultText>)>();
        q.iter(app.world()).any(|(s, t)| {
            *s == RunStatus::Completed
                && t.map(|t| t.0 == "escalated by fake model").unwrap_or(false)
        })
    };
    assert!(completed, "fake model should complete the escalation");
}

#[test]
fn rig_model_inbox_is_the_only_injection_point() {
    // I26：RigDriverState 字段私有——`AgentRun::model_response` 只能从
    // inbox drain 路径触达（编译期封死旁路）；I17：stale epoch 必须被丢弃。
    let mut app = App::new();
    app.add_plugins(BevyNeedlePlugin::with_backend(MockBackend::new(vec![json!({
        "type": "call", "success": true, "confidence": 0.01,
        "function_calls": [{ "name": "echo", "arguments": {} }],
    })])));
    app.world_mut().insert_resource(
        EscalationPolicy::new()
            .enabled()
            .with_fallback(OnlineFallback::LocalModelOnly)
            .with_tier(EscalationTarget::Local),
    );
    // 永不响应的模型：run 停在 awaiting=Model，stale 丢弃语义可被单独观察
    register_with_model(
        &mut app,
        DriverId("rig-never"),
        EscalationTarget::Local,
        Arc::new(FakeModel::never()),
        &[0],
    )
    .expect("rig driver registers");

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

    // 门控 → coordinator → RigDriver.submit（Attach）→ rig 系统接管
    run_frames(&mut app, 4);

    let run = {
        let w = app.world_mut();
        let mut q = w.query::<(Entity, &RunStatus, Option<&RigDriverState>)>();
        q.iter(w)
            .filter_map(|(run, _s, state)| state.is_some().then_some(run))
            .next()
            .expect("rig should have taken over")
    };

    // I17：stale epoch 回执必须被丢弃——run 不因此终态化
    app.world()
        .resource::<RigModelInbox<FakeModel>>()
        .inject(RigModelTurn {
            run,
            epoch: 999,
            outcome: Ok(rig_run::ModelTurn::new(
                None,
                vec![AssistantContent::text("stale should be dropped")],
                Usage::default(),
                BTreeSet::new(),
                BTreeSet::new(),
            )),
        });
    run_frames(&mut app, 3);

    let status = *app.world().get::<RunStatus>(run).expect("run alive");
    assert!(
        matches!(status, RunStatus::Escalating { .. }),
        "stale turn must not resolve run (got {status:?})"
    );
}
