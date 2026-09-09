//! 引擎信封类型、版本常量与库发现。
//!
//! 真正的引擎访问在 [`crate::backend::NeedleBackend`]（trait，可注入 mock）与
//! [`crate::ffi`]（唯一 unsafe）。本模块保持 100% 安全代码。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::NeedleError;

/// 引擎版本（上游 `fetch.ENGINE_VERSIONS[3]`；needle3 带 confidence head，
/// 库文件名带代际后缀，缓存路径按代际分轨）。
pub const ENGINE_VERSION: &str = "3.0.1";

/// `needle_complete` 的默认输出缓冲区大小（64 KiB，与 Python 绑定一致）。
pub const DEFAULT_BUFFER_SIZE: usize = 65536;

/// 引擎代际（needle2 / needle3，v14.4 适配）。
///
/// 上游从 needle3 起引入 generation 概念：库文件名带代际后缀
/// （`libneedle3.so`）、缓存路径按代际分轨（`~/.cache/cactus-needle/v3/`）、
/// 新增 `needle_embed` 符号、**基础权重必须显式 `needle_load`**（gen3 的
/// `.cact` 权重不再打包进库）。`.cact` 档案自带 generation tag
/// （0x05E12A83=2 / 0x05E12A84=3）——**v3 档案权重不能喂 v2 引擎**，反之亦然。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum EngineGeneration {
    /// Needle 3（唯一维护代际；`libneedle3.*`；需加载 `needle3.cact` 基础
    /// 权重；新增 `needle_embed` 符号与 confidence head）。
    #[default]
    Gen3,
}

impl EngineGeneration {
    /// 引擎版本常量。
    pub fn engine_version(self) -> &'static str {
        ENGINE_VERSION
    }

    /// 库文件名（带代际后缀——上游 `local_names = [f"{stem}{generation}{suffix}"]`）。
    pub fn library_file_name(self) -> &'static str {
        if cfg!(target_os = "macos") {
            "libneedle3.dylib"
        } else if cfg!(windows) {
            "libneedle3.dll"
        } else {
            "libneedle3.so"
        }
    }

    /// 缓存目录名（上游按代际分轨：`~/.cache/cactus-needle/v3/`）。
    pub fn cache_dir_name(self) -> Option<&'static str> {
        Some("v3")
    }

    /// 基础权重文件名（needle3 权重不打包在库内，必须显式 `needle_load`）。
    pub fn base_weights_file_name(self) -> Option<&'static str> {
        Some("needle3.cact")
    }
}

/// 引擎动态库文件名（按平台，带代际后缀）。
pub fn library_file_name() -> &'static str {
    EngineGeneration::Gen3.library_file_name()
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
/// 显式路径 → `NEEDLE_LIB_PATH`（⚠️ gen3 下上游会**忽略**此覆盖以防止 v3 权重
/// 被路由进 v2 引擎；本 crate 仍尊重显式路径的权威性——用户显式给定即负责）
/// → `NEEDLE_ENGINE_DIR` → 仓库 `third_party/needle/<version>/` → 可执行文件
/// 目录 → 缓存目录（gen2: `~/.cache/cactus-needle/<version>/`；
/// gen3: `~/.cache/cactus-needle/v3/<version>/`，上游按代际分轨）。
pub fn discover_library(explicit: Option<&Path>) -> Result<PathBuf, NeedleError> {
    discover_library_for(EngineGeneration::Gen3, explicit)
}

/// [`discover_library`] 的代际感知版本（v14.4：needle3 适配）。
pub fn discover_library_for(
    generation: EngineGeneration,
    explicit: Option<&Path>,
) -> Result<PathBuf, NeedleError> {
    let version = generation.engine_version();
    let lib_name = generation.library_file_name();
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
        let path = PathBuf::from(dir).join(lib_name);
        if path.is_file() {
            return Ok(path);
        }
        tried.push(path.display().to_string());
    }
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        candidates.push(
            Path::new(manifest)
                .join("../../third_party/needle")
                .join(version)
                .join(lib_name),
        );
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("third_party/needle").join(version).join(lib_name));
        candidates.push(dir.join(lib_name));
    }
    if let Some(home) = std::env::var_os("HOME") {
        // 上游按代际分轨：gen3 在 ~/.cache/cactus-needle/v3/<version>/
        let mut cache = PathBuf::from(home).join(".cache/cactus-needle");
        if let Some(sub) = generation.cache_dir_name() {
            cache = cache.join(sub);
        }
        candidates.push(cache.join(version).join(lib_name));
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Ok(candidate);
        }
        tried.push(candidate.display().to_string());
    }
    Err(NeedleError::EngineNotFound { tried: tried.join(", ") })
}

/// needle3 基础权重（`needle3.cact`）的默认发现路径
/// （与库发现同源：`third_party/needle/<version>/` → 缓存分轨 `v3/`）。
pub fn default_base_weights_path() -> PathBuf {
    let generation = EngineGeneration::Gen3;
    let file = generation
        .base_weights_file_name()
        .expect("needle3 has base weights");
    if let Some(manifest) = option_env!("CARGO_MANIFEST_DIR") {
        let p = Path::new(manifest)
            .join("../../third_party/needle")
            .join(generation.engine_version())
            .join(file);
        if p.is_file() {
            return p;
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let mut cache = PathBuf::from(home).join(".cache/cactus-needle");
        if let Some(sub) = generation.cache_dir_name() {
            cache = cache.join(sub);
        }
        let p = cache.join(generation.engine_version()).join(file);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(file)
}
