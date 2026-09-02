//! 推荐的日常导入面（对齐 bevy_rig 的 `prelude.rs`）。

pub use crate::{
    backend::{MockBackend, NeedleBackend},
    error::{NeedleError, NeedleRunError},
    agent::{
        attach_tool, detach_tool, primary_session, spawn_agent, AgentEngineOptions, AgentLinkError,
        AgentToolRefs, AgentHandles, NeedleAgentBundle, NeedleAgentSpec, PrimarySession,
    },
    app::{
        BevyNeedlePlugin, EngineConfig, EngineExecutionSystems, EngineSync, EngineSyncSystems,
        NeedleEngineConfig, NeedleEngineStatus, NeedleEngineStatusKind, RunCommit,
        RunCommitSystems, RunExecution, RunExecutionSystems, RunPreparation,
        RunPreparationSystems, RunResolutionSystems, Telemetry, ToolDispatchSystems,
    },
    diagnostics::RuntimeDiagnostics,
    engine::{
        discover_library, library_file_name, NeedleFunctionCall, NeedleResponse,
        DEFAULT_BUFFER_SIZE, ENGINE_VERSION,
    },
    engine_index::{rebuild_agent_tool_index, AgentToolIndex, AgentToolSnapshot},
    needle_runtime::{NeedleRuntime, RuntimeEvent, RuntimeJob, TurnJob},
    run::{
        cancel_runs, capture_run_requests, mark_run_completed, mark_run_failed,
        persist_cancelled_runs, persist_completed_runs, persist_failed_runs, CancelRun, ResetAgent,
        Run, RunAgent, RunAwaitingTools, RunBundle, RunCommitted, RunEngineInFlight, RunFailed,
        RunEscalation, RunExecutedResults, RunFailure, RunFinalized, RunLastResponse, RunNote, RunOwner,
        RunPendingInput, RunRequest, RunResultText, RunSession, RunStatus, RunTurn,
    },
    schema::{
        normalize_tool_schema, tools_json, ParametersBuilder, ToolSchemaError,
    },
    session::{
        collect_transcript, spawn_chat_message, spawn_chat_message_now, spawn_session,
        ChatMessage, ChatMessageBundle, ChatMessageRole, ChatMessageSeq, ChatMessageSession,
        ChatMessageText, Session, SessionBundle,
    },
    tool::{
        complete_tool_invocation, dispatch_registered_tool_calls, fail_tool_invocation,
        mark_tool_invocation_running, publish_tool_invocation_results,
        queue_requested_tool_calls, register_tool_handler, rebuild_tool_registry,
        RegisteredTool, Tool, ToolBundle, ToolCall, ToolCallCompleted, ToolCallFailed,
        ToolCallRequested, ToolDispatchPolicy, ToolExecutionError, ToolExecutionResult,
        ToolHandlerFn, ToolHandlers, ToolInvocation, ToolInvocationBundle, ToolInvocationCall,
        ToolInvocationError, ToolInvocationOutput, ToolInvocationPublished,
        ToolInvocationStatus, ToolInvocationTurn, ToolKind, ToolOutput, ToolRegistry, ToolSpec,
    },
};
