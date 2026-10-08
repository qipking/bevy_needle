//! `BevyNeedlePlugin`：调度表、系统集与 run 状态机推进（对齐 bevy_rig 的 `app.rs`）。
//!
//! 每帧管线：
//!
//! ```text
//! EngineSync          重建工具注册表与 agent 快照
//! RunPreparation      RunAgent 消息 → run 实体；处理取消
//! RunExecution        收割引擎事件 → 生成/回喂工具调用 → 提交下一轮
//! RunCommit           终态 run 落入会话转录
//! Telemetry           （预留）诊断刷新钩子
//! ```

use std::path::PathBuf;
use bevy_app::{App, MainScheduleOrder, Plugin, Update};
use bevy_ecs::{
    prelude::*,
    schedule::{IntoScheduleConfigs, Schedule, ScheduleLabel},
};

use crate::{
    backend::NeedleBackend,
    diagnostics::RuntimeDiagnostics,
    engine::discover_library,
    engine_index::{rebuild_agent_tool_index, AgentToolIndex},
    needle_runtime::{NeedleRuntime, RuntimeEvent, TurnJob},
    run::{
        cancel_runs, capture_run_requests, mark_run_completed, mark_run_escalated,
        mark_run_escalating, mark_run_failed, persist_cancelled_runs, persist_completed_runs,
        persist_escalated_runs, persist_failed_runs, CancelRun, ResetAgent, RunAgent,
        RunAwaitingTools, RunEngineInFlight, RunEscalated, RunEscalation, RunFinalized, RunNote,
        RunOwner, RunPendingInput, RunLastResponse, RunStatus, RunTurn,
    },
    tool::{
        dispatch_registered_tool_calls, publish_tool_invocation_results, rebuild_tool_registry,
        ToolCall, ToolHandlers, ToolInvocationBundle, ToolInvocationCall,
        ToolInvocationError, ToolInvocationOutput, ToolInvocationStatus, ToolInvocationTurn,
        ToolOutput, ToolRegistry,
    },
};

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
/// `EngineSync`（见类型级与模块级文档）。
pub struct EngineSync;

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
/// `RunPreparation`（见类型级与模块级文档）。
pub struct RunPreparation;

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
/// `RunExecution`（见类型级与模块级文档）。
pub struct RunExecution;

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
/// `RunCommit`（见类型级与模块级文档）。
pub struct RunCommit;

#[derive(ScheduleLabel, Clone, Debug, PartialEq, Eq, Hash)]
/// `Telemetry`（见类型级与模块级文档）。
pub struct Telemetry;

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
/// `EngineSyncSystems`（见类型级与模块级文档）。
pub struct EngineSyncSystems;

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
/// `RunPreparationSystems`（见类型级与模块级文档）。
pub struct RunPreparationSystems;

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
/// `EngineExecutionSystems`（见类型级与模块级文档）。
pub struct EngineExecutionSystems;

/// bevy_rig 风格的执行集（容纳引擎推进系统，便于游戏在其前后插入逻辑）。
#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
pub struct RunExecutionSystems;

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
/// `ToolDispatchSystems`（见类型级与模块级文档）。
pub struct ToolDispatchSystems;

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
/// `RunResolutionSystems`（见类型级与模块级文档）。
pub struct RunResolutionSystems;

#[derive(SystemSet, Clone, Debug, PartialEq, Eq, Hash)]
/// `RunCommitSystems`（见类型级与模块级文档）。
pub struct RunCommitSystems;


/// 引擎配置：显式库路径 / 调优权重 / 缓冲大小。
#[derive(Clone, Debug, Default)]
pub struct EngineConfig {
    /// `library_path`（语义见类型文档）。
    pub library_path: Option<PathBuf>,
    /// `weights_path`（语义见类型文档）。
    pub weights_path: Option<PathBuf>,
    /// `buffer_size`（语义见类型文档）。
    pub buffer_size: Option<usize>,
    /// needle3 基础权重路径（`needle3.cact`；None → 走发现路径
    /// `third_party/needle/3.0.1/` → 缓存分轨 `~/.cache/cactus-needle/v3/`）。
    pub base_weights_path: Option<PathBuf>,
}

impl EngineConfig {
    /// 指定引擎库路径。
    pub fn with_library(path: impl Into<PathBuf>) -> Self {
        Self {
            library_path: Some(path.into()),
            ..Self::default()
        }
    }

    /// 指定调优 `.cact` 权重（进程内一次性加载）。
    pub fn with_weights(mut self, path: impl Into<PathBuf>) -> Self {
        self.weights_path = Some(path.into());
        self
    }

    /// 显式指定 needle3 基础权重路径（缺省走发现路径）。
    pub fn with_base_weights(mut self, path: impl Into<PathBuf>) -> Self {
        self.base_weights_path = Some(path.into());
        self
    }
}

/// 引擎可用性状态。
#[derive(Resource, Clone, Debug)]
pub struct NeedleEngineStatus {
    /// 引擎路径（注入后端时为 None）。
    pub path: Option<PathBuf>,
    /// 引擎是否就绪。
    pub kind: NeedleEngineStatusKind,
}

/// 状态种类。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NeedleEngineStatusKind {
    /// 已注入自定义后端（mock / 测试 / 内嵌链接）。
    Injected,
    /// dlopen 打开成功。
    Ready,
    /// 不可用（附原因）。
    Unavailable,
}

/// 引擎资源状态（插件构建时同步校验）。
#[derive(Resource, Clone, Debug)]
pub struct NeedleEngineConfig(pub EngineConfig);

/// Bevy 插件。
///
/// ```no_run
/// use bevy_app::App;
/// use bevy_needle::prelude::*;
///
/// let mut app = App::new();
/// app.add_plugins(BevyNeedlePlugin::default());
/// ```
pub struct BevyNeedlePlugin {
    /// `config`（语义见类型文档）。
    pub config: EngineConfig,
    backend: Option<std::sync::Arc<dyn NeedleBackend>>,
}

impl Default for BevyNeedlePlugin {
    fn default() -> Self {
        Self {
            config: EngineConfig::default(),
            backend: None,
        }
    }
}

impl BevyNeedlePlugin {
    /// 显式引擎配置。
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            backend: None,
        }
    }

    /// 注入自定义后端（`MockBackend` / 测试用 / 内嵌链接引擎）。
    ///
    /// 注入后 `EngineConfig::library_path` 与权重加载被忽略——由后端
    /// 实现自行决定。注意：后端通过 `Arc` 共享，重复 add 同一插件会复用
    /// 同一后端实例。
    pub fn with_backend(backend: impl NeedleBackend) -> Self {
        Self {
            config: EngineConfig::default(),
            backend: Some(std::sync::Arc::new(backend)),
        }
    }
}

impl Plugin for BevyNeedlePlugin {
    fn build(&self, app: &mut App) {
        let config = {
            // 允许 app 在 add_plugins 之前插入 NeedleEngineConfig 覆盖默认值
            if let Some(existing) = app.world().get_resource::<NeedleEngineConfig>() {
                existing.0.clone()
            } else {
                self.config.clone()
            }
        };

        let buffer_size = config.buffer_size.unwrap_or(crate::engine::DEFAULT_BUFFER_SIZE);

        // 三分支：注入后端 / dlopen 打开 / 不可用降级。
        // 引擎缺失不是 panic：状态标 Unavailable，run 会以清晰的错误失败。
        let (status, runtime) = if let Some(backend) = self.backend.clone() {
            tracing::info!("needle: using injected backend");
            (
                NeedleEngineStatus {
                    path: None,
                    kind: NeedleEngineStatusKind::Injected,
                },
                Some(NeedleRuntime::new(backend)),
            )
        } else {
            match discover_library(config.library_path.as_deref()) {
                Ok(path) => {
                    #[cfg(feature = "dlopen")]
                    match crate::backend::DlopenBackend::open_with_base_weights(
                                &path,
                                config
                                    .base_weights_path
                                    .clone()
                                    .unwrap_or_else(crate::engine::default_base_weights_path),
                                buffer_size,
                            ) {
                        Ok(backend) => {
                            let backend: std::sync::Arc<dyn NeedleBackend> = std::sync::Arc::new(backend);
                            // 权重在启动时一次性加载（引擎无法卸载）
                            if let Some(weights) = &config.weights_path {
                                match std::fs::read(weights) {
                                    Ok(blob) => {
                                        if let Err(err) = backend.load_weights(&blob) {
                                            tracing::warn!("needle: 加载调优权重失败: {err}");
                                        }
                                    }
                                    Err(err) => {
                                        tracing::warn!("needle: 读取调优权重失败: {err}");
                                    }
                                }
                            }
                            tracing::info!(path = %path.display(), "needle: engine ready (version {})", crate::engine::ENGINE_VERSION);
                            (
                                NeedleEngineStatus {
                                    path: Some(path),
                                    kind: NeedleEngineStatusKind::Ready,
                                },
                                Some(NeedleRuntime::new(backend)),
                            )
                        }
                        Err(err) => {
                            tracing::warn!("needle: engine unavailable: {err}");
                            (
                                NeedleEngineStatus {
                                    path: Some(path),
                                    kind: NeedleEngineStatusKind::Unavailable,
                                },
                                None,
                            )
                        }
                    }
                    #[cfg(not(feature = "dlopen"))]
                    {
                        let _ = buffer_size;
                        tracing::warn!("needle: dlopen feature is disabled; engine unavailable");
                        (
                            NeedleEngineStatus {
                                path: Some(path),
                                kind: NeedleEngineStatusKind::Unavailable,
                            },
                            None,
                        )
                    }
                }
                Err(err) => {
                    tracing::warn!("needle: engine unavailable: {err}");
                    (
                        NeedleEngineStatus {
                            path: None,
                            kind: NeedleEngineStatusKind::Unavailable,
                        },
                        None,
                    )
                }
            }
        };

        app.init_resource::<ToolRegistry>()
            .init_resource::<ToolHandlers>()
            .init_resource::<AgentToolIndex>()
            .init_resource::<RuntimeDiagnostics>()
            .init_resource::<crate::policy::EscalationPolicy>()
            .insert_resource(status_for_diagnostics(&status))
            .insert_resource(status)
            .add_message::<RunAgent>()
            .add_message::<CancelRun>()
            .add_message::<ResetAgent>()
            .add_message::<crate::run::RunCommitted>()
            .add_message::<crate::run::RunFailed>()
            .add_message::<RunEscalation>()
            .add_message::<RunEscalated>()
            .add_message::<crate::tool::ToolCallRequested>()
            .add_message::<crate::tool::ToolCallCompleted>()
            .add_message::<crate::tool::ToolCallFailed>()
            .add_schedule(Schedule::new(EngineSync))
            .add_schedule(Schedule::new(RunPreparation))
            .add_schedule(Schedule::new(RunExecution))
            .add_schedule(Schedule::new(RunCommit))
            .add_schedule(Schedule::new(Telemetry))
            .configure_sets(EngineSync, EngineSyncSystems)
            .configure_sets(RunPreparation, RunPreparationSystems)
            .configure_sets(
                RunExecution,
                (
                    RunExecutionSystems,
                    ToolDispatchSystems,
                    RunResolutionSystems,
                )
                    .chain(),
            )
            .configure_sets(RunCommit, RunCommitSystems)
            .add_systems(
                EngineSync,
                (rebuild_tool_registry, rebuild_agent_tool_index).chain(),
            )
            .add_systems(
                RunPreparation,
                (capture_run_requests, cancel_runs)
                    .chain()
                    .in_set(RunPreparationSystems),
            )
            .add_systems(
                RunExecution,
                execute_needle_runs
                    .in_set(EngineExecutionSystems)
                    .in_set(RunExecutionSystems),
            )
            .add_systems(
                RunExecution,
                dispatch_registered_tool_calls.in_set(ToolDispatchSystems),
            )
            .add_systems(
                RunExecution,
                publish_tool_invocation_results.after(ToolDispatchSystems),
            )
            .add_systems(
                RunExecution,
                resolve_run_tool_turns.in_set(RunResolutionSystems),
            )
            .add_systems(
                RunExecution,
                finalize_escalations.in_set(RunResolutionSystems),
            )
            .add_systems(
                RunCommit,
                (
                    persist_completed_runs,
                    persist_failed_runs,
                    persist_cancelled_runs,
                    persist_escalated_runs,
                )
                    .in_set(RunCommitSystems),
            );

        // ── 升级能力层 / rig 实现层接线（规格 §3 接线约定）────────────────
        // coordinator 与 rig 步进系统都必须排在 finalize_escalations **之前**：
        // 后者会把所有 Escalating 收束为 Escalated（安全阀）。
        #[cfg(feature = "escalate")]
        {
            app.init_resource::<crate::escalate::DriverEventBus>()
                .init_resource::<crate::escalate::DriverRegistry>()
                .add_systems(
                    RunExecution,
                    crate::escalate::driver_coordinator
                        .in_set(RunResolutionSystems)
                        .before(finalize_escalations),
                );

            // rig 层的 inbox/系统是泛型（M: CompletionModel，RPITIT 非 dyn-compat），
            // 无法无具体类型注册——由宿主经 `rig::register_with_model<M>(..)` 接入
            // （见 rig/mod.rs；规格 §4.7 注册入口 + P1-7 裁决）。
        }

        if let Some(runtime) = runtime {
            app.insert_resource(runtime);
        }

        let mut order = app.world_mut().resource_mut::<MainScheduleOrder>();
        order.insert_after(Update, EngineSync);
        order.insert_after(EngineSync, RunPreparation);
        order.insert_after(RunPreparation, RunExecution);
        order.insert_after(RunExecution, RunCommit);
        order.insert_after(RunCommit, Telemetry);
    }
}

fn status_for_diagnostics(status: &NeedleEngineStatus) -> RuntimeDiagnostics {
    RuntimeDiagnostics {
        engine_path: status.path.clone(),
        engine_ready: status.kind != NeedleEngineStatusKind::Unavailable,
        engine_weights: None,
        ..RuntimeDiagnostics::default()
    }
}

/// 引擎执行系统：收割后台事件 + 处理重置请求 + 提交新的 turn（独占访问）。
pub fn execute_needle_runs(world: &mut World) {
    // 0) 处理会话重置请求
    let resets: Vec<ResetAgent> = {
        let mut messages = world.resource_mut::<Messages<ResetAgent>>();
        messages.drain().collect()
    };
    if !resets.is_empty() {
        if let Some(runtime) = world.get_resource::<NeedleRuntime>() {
            for _ in resets {
                runtime.submit_reset();
            }
        }
    }

    // 1) 收割完成的事件
    let events = match world.get_resource::<NeedleRuntime>() {
        Some(runtime) => runtime.drain_events(),
        None => Vec::new(),
    };

    for event in events {
        match event {
            RuntimeEvent::TurnCompleted { run, response } => {
                handle_turn_completed(world, run, response);
            }
            RuntimeEvent::TurnFailed { run, error } => {
                let running = world
                    .get::<RunStatus>(run)
                    .map(|status| *status == RunStatus::Running)
                    .unwrap_or(false);
                if let Ok(mut entity) = world.get_entity_mut(run) {
                    entity.remove::<RunEngineInFlight>();
                }
                if running {
                    mark_run_failed(world, run, error.clone());
                }
                let mut diagnostics = world.resource_mut::<RuntimeDiagnostics>();
                diagnostics.runs_failed += 1;
                diagnostics.last_error = Some(error);
            }
        }
    }

    // 2) 提交排队中的首轮 turn
    let pending: Vec<(Entity, RunPendingInput, crate::run::RunOwner)> = {
        let mut query = world.query::<(
            Entity,
            &RunPendingInput,
            &RunOwner,
            &RunStatus,
            Option<&RunEngineInFlight>,
            Option<&RunFinalized>,
        )>();
        query
            .iter(world)
            .filter(|(_, _, _, status, in_flight, finalized)| {
                **status == RunStatus::Queued && in_flight.is_none() && finalized.is_none()
            })
            .map(|(run, input, owner, _, _, _)| (run, input.clone(), owner.clone()))
            .collect()
    };

    if pending.is_empty() {
        return;
    }

    let submit = |world: &mut World, run: Entity, input: String, owner: Entity| -> bool {
        let Some(snapshot) = world
            .get_resource::<AgentToolIndex>()
            .and_then(|index| index.get(owner))
            .cloned()
        else {
            mark_run_failed(world, run, "agent 没有可用的引擎快照（工具 schema 错误？）");
            return false;
        };
        let Some(runtime) = world.get_resource::<NeedleRuntime>() else {
            mark_run_failed(
                world,
                run,
                world
                    .resource::<RuntimeDiagnostics>()
                    .last_error
                    .clone()
                    .unwrap_or_else(|| "引擎不可用".into()),
            );
            return false;
        };
        let ok = runtime.submit_turn(TurnJob {
            run,
            signature: snapshot.signature,
            system: std::sync::Arc::from(snapshot.system.as_str()),
            tools_json: std::sync::Arc::from(snapshot.tools_json.as_str()),
            tool_index: snapshot.tool_index_path.clone(),
            input: std::sync::Arc::from(input.as_str()),
            max_new_tokens: snapshot.max_new_tokens,
            buffer_size: snapshot.buffer_size,
        });
        let mut diagnostics = world.resource_mut::<RuntimeDiagnostics>();
        diagnostics.channel_broken |= !ok;
        ok
    };

    for (run, pending_input, owner) in pending {
        let input = pending_input.0;
        if submit(world, run, input, owner.0) {
            if let Ok(mut entity) = world.get_entity_mut(run) {
                entity.insert(RunStatus::Running).insert(RunEngineInFlight);
            }
            world.resource_mut::<RuntimeDiagnostics>().runs_started += 1;
        }
    }
}

/// 处理一次完成的引擎 turn：结束 run 或生成工具调用。
fn handle_turn_completed(world: &mut World, run: Entity, response: crate::engine::NeedleResponse) {
    // 若 run 已被取消/终结，直接丢弃
    let cancelled = world
        .get::<RunStatus>(run)
        .map(|status| !matches!(status, RunStatus::Running))
        .unwrap_or(true);
    if let Ok(mut entity) = world.get_entity_mut(run) {
        entity.remove::<RunEngineInFlight>();
    }
    if cancelled {
        return;
    }

    {
        let mut diagnostics = world.resource_mut::<RuntimeDiagnostics>();
        diagnostics.turns_completed += 1;
        diagnostics.last_confidence = response.confidence;
        diagnostics.last_decode_tps = response.decode_tps;
        diagnostics.last_prefill_tps = response.prefill_tps;
        diagnostics.peak_ram_mb = response.peak_ram_mb.or(diagnostics.peak_ram_mb);
    }

    let response_value = serde_json::to_value(&response).ok();
    if let Ok(mut entity) = world.get_entity_mut(run) {
        entity.insert(RunLastResponse(response_value));
    }

    let snapshot = {
        let owner = world.get::<crate::run::RunOwner>(run).map(|o| o.0);
        owner.and_then(|owner| {
            world
                .get_resource::<AgentToolIndex>()
                .and_then(|index| index.get(owner))
                .cloned()
        })
    };
    let Some(snapshot) = snapshot else {
        mark_run_failed(world, run, "引擎快照丢失");
        return;
    };

    // 引擎明确报错
    if !response.success {
        let message = response
            .error
            .clone()
            .unwrap_or_else(|| "engine reported failure".into());
        world.resource_mut::<RuntimeDiagnostics>().runs_failed += 1;
        mark_run_failed(world, run, message);
        return;
    }

    let calls = &response.function_calls;

    // 无调用：回合结束（Needle 约定：type=respond 或空调用即完成）。
    // 置信度门控只约束“是否执行调用”——最终答复轮不参与门控。
    if calls.is_empty() || response.kind != "call" {
        let summary = response.summary();
        world.resource_mut::<RuntimeDiagnostics>().runs_completed += 1;
        mark_run_completed(world, run, summary);
        return;
    }

    if let (Some(threshold), Some(confidence)) =
        (snapshot.confidence_threshold, response.confidence)
    {
        // f64/f32 cast 的唯一入口（规格 §10）：比较收敛到 EscalationPolicy。
        let below = world
            .resource::<crate::policy::EscalationPolicy>()
            .below_threshold(confidence, threshold);
        if below {
            // Needle 契约：低于门限的调用**不执行**，run 进入升级语义。
            // 升级是契约行为而非失败——Failed 只留给引擎/调度错误。
            // 门控只约束"有调用"的轮：最终答复轮不参与门控（README 实测警告）。
            world.write_message(RunEscalation {
                run,
                confidence,
                threshold,
            });
            world.resource_mut::<RuntimeDiagnostics>().runs_escalated += 1;
            mark_run_escalating(
                world,
                run,
                0,
                format!(
                    "置信度 {confidence:.2} 低于门限 {threshold:.2}，调用未执行（升级契约）"
                ),
            );
            return;
        }
    }

    // 步数上限检查（max_steps 限制工具执行轮数）
    let turn = world.get::<RunTurn>(run).map(|t| t.0).unwrap_or(0);
    if turn >= snapshot.max_steps {
        let summary = response.summary();
        if let Ok(mut entity) = world.get_entity_mut(run) {
            entity.insert(RunNote("已达到 max_steps 上限，停止继续调用".into()));
        }
        world.resource_mut::<RuntimeDiagnostics>().runs_completed += 1;
        mark_run_completed(world, run, summary);
        return;
    }

    // 物化工具调用实体（registry 借用在此作用域内结束）
    let mut expected = 0u32;
    let mut spawned: Vec<(ToolCall, Option<String>)> = Vec::new();

    {
        let registry = world.resource::<ToolRegistry>();
        for call in &response.function_calls {
            let tool_entity = registry.get_by_name(&call.name);
            let tool_call = ToolCall::new(
                run,
                tool_entity.unwrap_or(Entity::PLACEHOLDER),
                call.name.clone(),
                call.arguments.clone(),
            );
            let immediate_error = match tool_entity {
                None => Some(format!("unknown tool: {}", call.name)),
                Some(_) => None,
            };
            spawned.push((tool_call, immediate_error));
            expected += 1;
        }
    }

    {
        let current_turn = turn;
        for (tool_call, immediate_error) in &spawned {
            let invocation = world
                .spawn(ToolInvocationBundle::new(tool_call.clone()))
                .insert(ToolInvocationTurn(current_turn))
                .id();
            if let Some(error) = immediate_error {
                crate::tool::fail_tool_invocation(world, invocation, error.clone());
            }
            world.write_message(crate::tool::ToolCallRequested {
                call: tool_call.clone(),
            });
        }
    }

    let mut diagnostics = world.resource_mut::<RuntimeDiagnostics>();
    diagnostics.tool_calls_total += expected as u64;
    drop(diagnostics);

    if let Ok(mut entity) = world.get_entity_mut(run) {
        // RunTurn 保持为“当前轮”；resolve 回喂下一轮时才自增
        entity.insert(RunAwaitingTools { expected });
    }
}

/// 安全阀（不变量 I9）：未被任何 driver 认领的 `Escalating` 收束为 `Escalated`
/// （「没试/不许试」，规格 §6），绝不静默悬挂。
///
/// 已被认领（存在 `EscalationState`）的升级**不归本阀管**——那是 coordinator
/// 的在途 attempt，由其 deadline / 总预算 / 终态规则保证收敛（规格 §7）。
/// 无 `escalate` feature 时不存在 coordinator，所有 Escalating 都是未认领的。
pub fn finalize_escalations(world: &mut World) {
    #[cfg(feature = "escalate")]
    let escalating: Vec<(Entity, Option<String>)> = {
        let mut query = world.query::<(
            Entity,
            &RunStatus,
            Option<&crate::escalate::EscalationState>,
            Option<&RunNote>,
        )>();
        query
            .iter(world)
            .filter_map(|(run, status, claimed, note)| match status {
                RunStatus::Escalating { tier: _ } if claimed.is_none() => {
                    Some((run, note.map(|n| n.0.clone())))
                }
                _ => None,
            })
            .collect()
    };

    #[cfg(not(feature = "escalate"))]
    let escalating: Vec<(Entity, Option<String>)> = {
        let mut query = world.query::<(Entity, &RunStatus, Option<&RunNote>)>();
        query
            .iter(world)
            .filter_map(|(run, status, note)| match status {
                RunStatus::Escalating { tier: _ } => Some((run, note.map(|n| n.0.clone()))),
                _ => None,
            })
            .collect()
    };

    for (run, note) in escalating {
        mark_run_escalated(
            world,
            run,
            note.unwrap_or_else(|| "升级流程终结（当前无更多档位）".into()),
        );
    }
}

/// 解析当前轮工具结果：全部终态后把结果 JSON 回喂引擎（或收尾 run）。
pub fn resolve_run_tool_turns(world: &mut World) {
    let awaiting: Vec<(Entity, u32, u32)> = {
        let mut query = world.query::<(Entity, &RunAwaitingTools, &RunStatus, &RunTurn)>();
        query
            .iter(world)
            .filter(|(_, _, status, _)| **status == RunStatus::Running)
            .map(|(run, awaiting, _, turn)| (run, awaiting.expected, turn.0))
            .collect()
    };

    if awaiting.is_empty() {
        return;
    }

    for (run, expected, current_turn) in awaiting {
        let mut query = world.query::<(
            Entity,
            &ToolInvocationCall,
            &ToolInvocationStatus,
            &ToolInvocationTurn,
            Option<&ToolInvocationOutput>,
            Option<&ToolInvocationError>,
        )>();
        let mut rows: Vec<(u64, ToolCall, ToolInvocationStatus, ToolOutput, Option<String>)> =
            query
                .iter(world)
                .filter(|(_, call, _, invocation_turn, _, _)| {
                    call.0.run == run && invocation_turn.0 == current_turn
                })
                .map(|(entity, call, status, _, output, error)| {
                    (
                        entity.to_bits(),
                        call.0.clone(),
                        *status,
                        output.map(|o| o.0.clone()).unwrap_or_default(),
                        error.map(|e| e.0.clone()),
                    )
                })
                .collect();
        rows.sort_by_key(|(bits, _, _, _, _)| *bits);

        let terminal = rows
            .iter()
            .filter(|(_, _, status, _, _)| {
                matches!(
                    status,
                    ToolInvocationStatus::Completed | ToolInvocationStatus::Failed
                )
            })
            .count();

        if terminal < expected as usize {
            continue; // 还有调用在途（External 策略）
        }

        // 组装结果数组（Needle 约定：错误以 {"error": ...} 喂回）
        let results: Vec<serde_json::Value> = rows
            .iter()
            .map(|(_, _call, status, output, error)| match status {
                ToolInvocationStatus::Completed => output.value.clone(),
                _ => serde_json::json!({ "error": error.clone().unwrap_or_default() }),
            })
            .collect();
        let completed_count = rows
            .iter()
            .filter(|(_, _, status, _, _)| *status == ToolInvocationStatus::Completed)
            .count();
        let failed_count = rows.len() - completed_count;
        {
            let mut diagnostics = world.resource_mut::<RuntimeDiagnostics>();
            diagnostics.tool_calls_completed += completed_count as u64;
            diagnostics.tool_calls_failed += failed_count as u64;
        }

        if let Ok(mut entity) = world.get_entity_mut(run) {
            entity.remove::<RunAwaitingTools>();
            let mut executed = entity
                .get::<crate::run::RunExecutedResults>()
                .map(|r| r.0.clone())
                .unwrap_or_default();
            executed.extend(results.iter().cloned());
            entity.insert(crate::run::RunExecutedResults(executed));
        }

        // 失败的调用也回喂错误让模型自恢复（与 Python run() 一致）
        let input = serde_json::to_string(&results).unwrap_or_else(|_| "[]".into());
        let owner = world.get::<crate::run::RunOwner>(run).map(|o| o.0);
        let Some(owner) = owner else {
            mark_run_failed(world, run, "run 丢失 owner");
            continue;
        };
        let Some(snapshot) = world
            .get_resource::<AgentToolIndex>()
            .and_then(|index| index.get(owner))
            .cloned()
        else {
            mark_run_failed(world, run, "引擎快照丢失");
            continue;
        };
        let Some(runtime) = world.get_resource::<NeedleRuntime>() else {
            mark_run_failed(world, run, "引擎运行时不可用（backend 未注入）");
            continue;
        };
        let ok = runtime.submit_turn(TurnJob {
            run,
            signature: snapshot.signature,
            system: std::sync::Arc::from(snapshot.system.as_str()),
            tools_json: std::sync::Arc::from(snapshot.tools_json.as_str()),
            tool_index: snapshot.tool_index_path.clone(),
            input: std::sync::Arc::from(input.as_str()),
            max_new_tokens: snapshot.max_new_tokens,
            buffer_size: snapshot.buffer_size,
        });
        if ok {
            if let Ok(mut entity) = world.get_entity_mut(run) {
                entity.insert(RunEngineInFlight).insert(RunTurn(current_turn + 1));
            }
        } else {
            world.resource_mut::<RuntimeDiagnostics>().channel_broken = true;
            mark_run_failed(world, run, "引擎通道断开");
        }
    }
}
