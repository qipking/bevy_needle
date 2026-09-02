//! 运行时诊断：引擎状态 + run/turn/工具调用计数（对齐 bevy_rig 的 `diagnostics.rs`）。

use std::path::PathBuf;

use bevy_ecs::prelude::*;

/// 由各系统在事件发生时递增；游戏可在 Telemetry 阶段或 Update 里读取。
#[derive(Resource, Debug, Clone, Default)]
pub struct RuntimeDiagnostics {
    /// `engine_path`（语义见类型文档）。
    pub engine_path: Option<PathBuf>,
    /// `engine_ready`（语义见类型文档）。
    pub engine_ready: bool,
    /// `engine_weights`（语义见类型文档）。
    pub engine_weights: Option<PathBuf>,
    /// `runs_started`（语义见类型文档）。
    pub runs_started: u64,
    /// `runs_completed`（语义见类型文档）。
    pub runs_completed: u64,
    /// `runs_failed`（语义见类型文档）。
    pub runs_failed: u64,
    /// `runs_escalated`（语义见类型文档）。
    pub runs_escalated: u64,
    /// `runs_cancelled`（语义见类型文档）。
    pub runs_cancelled: u64,
    /// `turns_completed`（语义见类型文档）。
    pub turns_completed: u64,
    /// `tool_calls_total`（语义见类型文档）。
    pub tool_calls_total: u64,
    /// `tool_calls_completed`（语义见类型文档）。
    pub tool_calls_completed: u64,
    /// `tool_calls_failed`（语义见类型文档）。
    pub tool_calls_failed: u64,
    /// `last_confidence`（语义见类型文档）。
    pub last_confidence: Option<f64>,
    /// `last_decode_tps`（语义见类型文档）。
    pub last_decode_tps: Option<f64>,
    /// `last_prefill_tps`（语义见类型文档）。
    pub last_prefill_tps: Option<f64>,
    /// `peak_ram_mb`（语义见类型文档）。
    pub peak_ram_mb: Option<f64>,
    /// `channel_broken`（语义见类型文档）。
    pub channel_broken: bool,
    /// `last_error`（语义见类型文档）。
    pub last_error: Option<String>,
}

impl RuntimeDiagnostics {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn summary(&self) -> String {
        format!(
            "engine={} runs(✓{} ✗{} ⇧{} ⊘{}) turns={} tools(Σ{} ✓{} ✗{}) conf={} decode_tps={}",
            if self.engine_ready { "ready" } else { "down" },
            self.runs_completed,
            self.runs_failed,
            self.runs_escalated,
            self.runs_cancelled,
            self.turns_completed,
            self.tool_calls_total,
            self.tool_calls_completed,
            self.tool_calls_failed,
            self.last_confidence
                .map(|c| format!("{c:.2}"))
                .unwrap_or_else(|| "-".into()),
            self.last_decode_tps
                .map(|t| format!("{t:.0}"))
                .unwrap_or_else(|| "-".into()),
        )
    }
}
