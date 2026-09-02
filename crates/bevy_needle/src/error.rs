//! 错误类型：引擎层（FFI/信封）与运行层（调度）分离。

use std::path::PathBuf;

/// 引擎与信封层错误（FFI 调用、JSON 解析、权重加载）。
#[derive(Debug, thiserror::Error)]
pub enum NeedleError {
    /// 找不到引擎动态库（附尝试过的候选路径）。
    #[error("needle engine library not found (tried: {tried})")]
    EngineNotFound {
        /// 全部尝试过的候选路径，便于诊断。
        tried: String,
    },
    /// dlopen 失败。
    #[error("failed to load needle engine {path}: {reason}")]
    LibraryLoad {
        /// 引擎路径。
        path: PathBuf,
        /// libloading 底层错误文本。
        reason: String,
    },
    /// 缺失导出符号（引擎版本与 ABI 不匹配）。
    #[error("engine is missing symbol {symbol:?}: {reason}")]
    MissingSymbol {
        /// 符号名。
        symbol: &'static str,
        /// libloading 底层错误文本。
        reason: String,
    },
    /// `needle_init` 返回负值。
    #[error("needle_init failed (code {0})")]
    InitFailed(i32),
    /// `needle_complete` 返回负值。
    #[error("needle_complete failed (code {0})")]
    CompleteFailed(i32),
    /// `needle_load` 返回非零。
    #[error("needle_load failed (code {0})")]
    LoadWeightsFailed(i32),
    /// 字符串含内部 NUL，无法转 C 字符串。
    #[error("string field {field:?} contains an interior NUL")]
    InteriorNul {
        /// 出错字段名（诊断用）。
        field: &'static str,
    },
    /// 引擎写入的缓冲没有 NUL 结尾（缓冲太小或引擎异常）。
    #[error("engine output buffer was not NUL-terminated (buffer too small?)")]
    BufferNotTerminated,
    /// 引擎信封不是合法 UTF-8。
    #[error("engine envelope is not valid UTF-8: {0}")]
    BadUtf8(#[source] std::str::Utf8Error),
    /// 引擎信封 JSON 解析失败（附原始载荷片段）。
    #[error("engine envelope is not valid JSON: {source}; raw payload: {raw}")]
    BadEnvelope {
        /// serde 错误。
        source: serde_json::Error,
        /// 原始载荷片段（截断到 512 字节）。
        raw: String,
    },
    /// 读调优权重文件失败。
    #[error("failed to read weights file {path}: {source}")]
    WeightsIo {
        /// 权重路径。
        path: PathBuf,
        /// IO 错误。
        source: std::io::Error,
    },
}

impl NeedleError {
    /// 把引擎信封（可能是乱码）截断成安全的日志片段。
    pub fn bad_envelope(source: serde_json::Error, raw: &[u8]) -> Self {
        let raw = String::from_utf8_lossy(raw);
        let raw = raw.chars().take(512).collect();
        NeedleError::BadEnvelope { source, raw }
    }
}

/// 调度层错误（run 级失败原因，附着在 `RunFailure` 上）。
#[derive(Debug, thiserror::Error)]
pub enum NeedleRunError {
    /// agent 缺少引擎快照（工具 schema 非法或未注册）。
    #[error("agent has no engine snapshot: {0}")]
    NoSnapshot(String),
    /// 引擎不可用（未加载/加载失败）。
    #[error("engine unavailable: {0}")]
    EngineUnavailable(String),
    /// 工作线程通道断开。
    #[error("engine worker channel is closed")]
    ChannelClosed,
    /// 置信度低于门限（Needle 契约：升级而不是执行）。
    #[error("confidence {confidence:.3} below threshold {threshold:.3}: escalate, do not execute")]
    BelowConfidence {
        /// 本次调用置信度。
        confidence: f64,
        /// agent 配置的门限。
        threshold: f32,
    },
    /// 引擎显式报错。
    #[error("engine reported failure: {0}")]
    Engine(String),
}
