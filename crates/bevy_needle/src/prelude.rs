//! 推荐的日常导入面（对齐 bevy_rig 的 `prelude.rs`）。

pub use crate::{
    agent::{
        AgentEngineOptions, AgentHandles, AgentLinkError, AgentToolRefs, NeedleAgentBundle,
        NeedleAgentSpec, PrimarySession, attach_tool, detach_tool, primary_session, spawn_agent,
    },
    app::{
        BevyNeedlePlugin, EngineConfig, EngineExecutionSystems, EngineSync, EngineSyncSystems,
        NeedleEngineConfig, NeedleEngineStatus, NeedleEngineStatusKind, RunCommit,
        RunCommitSystems, RunExecution, RunExecutionSystems, RunPreparation, RunPreparationSystems,
        RunResolutionSystems, Telemetry, ToolDispatchSystems,
    },
    backend::{MockBackend, NeedleBackend},
    diagnostics::RuntimeDiagnostics,
    engine::{
        DEFAULT_BUFFER_SIZE, ENGINE_VERSION, EngineGeneration, NeedleFunctionCall, NeedleResponse,
        discover_library, library_file_name,
    },
    engine_index::{AgentToolIndex, AgentToolSnapshot, rebuild_agent_tool_index},
    error::{NeedleError, NeedleRunError},
    needle_runtime::{NeedleRuntime, RuntimeEvent, RuntimeJob, TurnJob},
    policy::{EscalationPolicy, EscalationTarget, OnlineFallback},
    run::{
        CancelRun, ResetAgent, Run, RunAgent, RunAwaitingTools, RunBundle, RunCommitted,
        RunEngineInFlight, RunEscalated, RunEscalation, RunExecutedResults, RunFailed, RunFailure,
        RunFinalized, RunLastResponse, RunNote, RunOwner, RunPendingInput, RunRequest,
        RunResultText, RunSession, RunStatus, RunTurn, cancel_runs, capture_run_requests,
        mark_run_completed, mark_run_escalated, mark_run_escalating, mark_run_failed,
        persist_cancelled_runs, persist_completed_runs, persist_escalated_runs,
        persist_failed_runs,
    },
    schema::{ParametersBuilder, ToolSchemaError, normalize_tool_schema, tools_json},
    session::{
        ChatMessage, ChatMessageBundle, ChatMessageRole, ChatMessageSeq, ChatMessageSession,
        ChatMessageText, Session, SessionBundle, collect_transcript, spawn_chat_message,
        spawn_chat_message_now, spawn_session,
    },
    tool::{
        RegisteredTool, Tool, ToolBundle, ToolCall, ToolCallCompleted, ToolCallFailed,
        ToolCallRequested, ToolDispatchPolicy, ToolExecutionError, ToolExecutionResult,
        ToolHandlerFn, ToolHandlers, ToolInvocation, ToolInvocationBundle, ToolInvocationCall,
        ToolInvocationError, ToolInvocationOutput, ToolInvocationPublished, ToolInvocationStatus,
        ToolInvocationTurn, ToolKind, ToolOutput, ToolRegistry, ToolSpec, complete_tool_invocation,
        dispatch_registered_tool_calls, fail_tool_invocation, mark_tool_invocation_running,
        publish_tool_invocation_results, queue_requested_tool_calls, rebuild_tool_registry,
        register_tool_handler,
    },
};
