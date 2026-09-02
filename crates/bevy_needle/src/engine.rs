//! 引擎信封类型、版本常量与库发现。
//!
//! 真正的引擎访问在 [`crate::backend::NeedleBackend`]（trait，可注入 mock）与
//! [`crate::ffi`]（唯一 unsafe）。本模块保持 100% 安全代码。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::NeedleError;

/// 引擎版本（对应上游 `needle.agent.fetch.ENGINE_VERSION`；`.cact` 权重格式与其绑定）。
pub const ENGINE_VERSION: &str = "2.0.3";

/// `needle_complete` 的默认输出缓冲区大小（64 KiB，与 Python 绑定一致）。
pub const DEFAULT_BUFFER_SIZE: usize = 65536;

/// 引擎动态库文件名（按平台）。
pub fn library_file_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "libneedle.dylib"
    } else if cfg!(windows) {
        "libneedle.dll"
    } else {
        "libneedle.so"
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
/// `NeedleFunctionCall`（见类型级与模块级文档）。
pub struct NeedleFunctionCall {
    /// `name`（语义见类型文档）。
    pub name: String,
    #[serde(default)]
    /// `arguments`（语义见类型文档）。
    pub arguments: serde_json::Value,
}

/// 引擎每次 `complete` 返回的 JSON 信封。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NeedleResponse {
    #[serde(rename = "type")]
    /// `kind`（语义见类型文档）。
    pub kind: String,
    #[serde(default)]
    /// `success`（语义见类型文档）。
    pub success: bool,
    #[serde(default)]
    /// `error`（语义见类型文档）。
    pub error: Option<String>,
    #[serde(default)]
    /// `error_code`（语义见类型文档）。
    pub error_code: Option<String>,
    #[serde(default)]
    /// `function_calls`（语义见类型文档）。
    pub function_calls: Vec<NeedleFunctionCall>,
    #[serde(default)]
    /// `reasoning`（语义见类型文档）。
    pub reasoning: Option<String>,
    #[serde(default)]
    /// `confidence`（语义见类型文档）。
    pub confidence: Option<f64>,
    #[serde(default)]
    /// `prefill_tps`（语义见类型文档）。
    pub prefill_tps: Option<f64>,
    #[serde(default)]
    /// `decode_tps`（语义见类型文档）。
    pub decode_tps: Option<f64>,
    #[serde(default)]
    /// `peak_ram_mb`（语义见类型文档）。
    pub peak_ram_mb: Option<f64>,
    #[serde(default)]
    /// `validation`（语义见类型文档）。
    pub validation: Option<serde_json::Value>,
}

impl NeedleResponse {
    /// 模型是否给出了可执行的工具调用。
    pub fn is_call(&self) -> bool {
        self.kind == "call" && !self.function_calls.is_empty()
    }

    /// 人工可读的结果摘要（模型不做自由文本生成，只有推导链）。
    pub fn summary(&self) -> String {
        if let Some(reasoning) = &self.reasoning {
            return reasoning.clone();
        }
        if self.is_call() {
            let names: Vec<&str> = self
                .function_calls
                .iter()
                .map(|call| call.name.as_str())
                .collect();
            return names.join(", ");
        }
        String::new()
    }

    /// 从字节切片解析信封（UTF-8 + JSON）。
    pub fn parse(bytes: &[u8]) -> Result<Self, NeedleError> {
        let raw = std::str::from_utf8(bytes).map_err(NeedleError::BadUtf8)?;
        serde_json::from_str(raw).map_err(|source| NeedleError::bad_envelope(source, bytes))
    }
}

/// 按约定顺序搜索引擎库：
/// 显式路径 → `NEEDLE_LIB_PATH` → `NEEDLE_ENGINE_DIR` → 仓库
/// `third_party/needle/<version>/` → 可执行文件目录 → `~/.cache/cactus-needle/<version>/`。
pub fn discover_library(explicit: Option<&Path>) -> Result<PathBuf, NeedleError> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let mut tried: Vec<String> = Vec::new();

    // 显式路径是权威的：给了路径但缺失 → 立即报错，不静默回退
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(NeedleError::EngineNotFound {
            tried: path.display().to_string(),
        });
    }
    if let Ok(env_path) = std::env::var("NEEDLE_LIB_PATH") {
        let path = PathBuf::from(env_path);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(path.display().to_string());
    }
    if let Ok(dir) = std::env::var("NEEDLE_ENGINE_DIR") {
        let path = PathBuf::from(dir).join(library_file_name());
        if path.is_file() {
            return Ok(path);
        }
        tried.push(path.display().to_string());
    }
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(
            Path::new(manifest)
                .join("../../third_party/needle")
                .join(ENGINE_VERSION)
                .join(library_file_name()),
        );
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(
            dir.join("third_party/needle")
                .join(ENGINE_VERSION)
                .join(library_file_name()),
        );
        candidates.push(dir.join(library_file_name()));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".cache/cactus-needle")
                .join(ENGINE_VERSION)
                .join(library_file_name()),
        );
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    Err(NeedleError::EngineNotFound { tried: tried.join(", ") })
}
