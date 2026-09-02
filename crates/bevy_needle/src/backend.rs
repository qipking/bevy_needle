//! 引擎后端抽象：真实 FFI 后端与脚本化 Mock 后端共用一个接口。
//!
//! 这是可测试性的关键：调度器（[`crate::needle_runtime`]）只认 trait，
//! 因此完整的轮次循环可以在**没有任何动态库**的环境下用 [`MockBackend`]
//! 做单元测试（脚本化“下一轮返回什么信封”）。
//!
//! 线程模型：真实后端的 `complete` 是阻塞调用（毫秒~秒级），由工作线程
//! 串行执行；`Send + Sync` 约束写在 trait 上，Mock 亦满足。

use std::sync::Mutex;

use serde_json::Value;

use crate::engine::{NeedleResponse, DEFAULT_BUFFER_SIZE};
use crate::error::NeedleError;

/// 引擎访问 trait。
///
/// `bind` 对应 `needle_init`（切换 system facts/工具集会重置 KV 会话），
/// `complete` 对应一次约束解码轮。
pub trait NeedleBackend: Send + Sync + 'static {
    /// 绑定（或重绑）当前会话。实现方可缓存签名，重复 bind 无副作用。
    ///
    /// # Errors
    /// 引擎初始化失败（工具 JSON 非法、引擎内部错误等）。
    fn bind(
        &self,
        signature: u64,
        system: &str,
        tools_json: &str,
        tool_index: Option<&std::path::Path>,
    ) -> Result<(), NeedleError>;

    /// 阻塞执行一轮解码；`buffer` 为输出缓冲（容量见 [`Self::buffer_size`]）。
    ///
    /// # Errors
    /// 解码失败或信封解析失败。
    fn complete(
        &self,
        input: &str,
        max_new_tokens: u32,
        buffer: &mut [u8],
    ) -> Result<NeedleResponse, NeedleError>;

    /// 回退当前会话（工具集保留）。
    fn reset(&self);

    /// 进程内一次性加载调优 `.cact` 权重。
    ///
    /// # Errors
    /// 权重读取或加载失败。
    fn load_weights(&self, blob: &[u8]) -> Result<(), NeedleError> {
        let _ = blob;
        Ok(())
    }

    /// 推荐的输出缓冲大小。
    fn buffer_size(&self) -> usize {
        DEFAULT_BUFFER_SIZE
    }
}

/// 真实引擎后端：[`crate::ffi::FfiEngine`] + 线程安全的绑定签名缓存。
#[cfg(feature = "dlopen")]
pub struct DlopenBackend {
    ffi: crate::ffi::FfiEngine,
    bound: Mutex<Option<u64>>,
    buffer_size: usize,
}

#[cfg(feature = "dlopen")]
impl std::fmt::Debug for DlopenBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DlopenBackend").finish_non_exhaustive()
    }
}

#[cfg(feature = "dlopen")]
impl DlopenBackend {
    /// 打开引擎并包装成后端。
    ///
    /// # Errors
    /// 库加载或符号解析失败。
    pub fn open(path: &std::path::Path, buffer_size: usize) -> Result<Self, NeedleError> {
        Ok(Self {
            ffi: crate::ffi::FfiEngine::open(path)?,
            bound: Mutex::new(None),
            buffer_size: buffer_size.max(1024),
        })
    }
}

#[cfg(feature = "dlopen")]
impl NeedleBackend for DlopenBackend {
    fn bind(
        &self,
        signature: u64,
        system: &str,
        tools_json: &str,
        tool_index: Option<&std::path::Path>,
    ) -> Result<(), NeedleError> {
        {
            let bound = self.bound.lock().expect("bound poisoned");
            if *bound == Some(signature) {
                return Ok(());
            }
        }
        let index = tool_index.map(|p| p.display().to_string());
        self.ffi.init(system, tools_json, index.as_deref())?;
        *self.bound.lock().expect("bound poisoned") = Some(signature);
        Ok(())
    }

    fn complete(
        &self,
        input: &str,
        max_new_tokens: u32,
        buffer: &mut [u8],
    ) -> Result<NeedleResponse, NeedleError> {
        // 失败后强制下次重绑（引擎状态可能已被污染）。
        match self.ffi.complete(input, max_new_tokens, buffer) {
            Ok(len) => NeedleResponse::parse(&buffer[..len]),
            Err(err) => {
                if let Ok(mut bound) = self.bound.lock() {
                    *bound = None;
                }
                Err(err)
            }
        }
    }

    fn reset(&self) {
        self.ffi.reset();
        if let Ok(mut bound) = self.bound.lock() {
            *bound = None; // reset 语义：下次强制重新 init
        }
    }

    fn load_weights(&self, blob: &[u8]) -> Result<(), NeedleError> {
        self.ffi.load_weights(blob)?;
        if let Ok(mut bound) = self.bound.lock() {
            *bound = None; // 权重变化后必须重新 init
        }
        Ok(())
    }

    fn buffer_size(&self) -> usize {
        self.buffer_size
    }
}

/// 脚本化 Mock 后端：按顺序回放信封，或用闭包动态生成。
///
/// ```no_run
/// use bevy_needle::backend::MockBackend;
/// use serde_json::json;
///
/// let mock = MockBackend::new(vec![json!({
///     "type": "call",
///     "success": true,
///     "function_calls": [{ "name": "echo", "arguments": { "text": "hi" } }],
/// })]);
/// // 提交给插件：BevyNeedlePlugin::with_backend(mock)
/// ```
pub struct MockBackend {
    script: Mutex<Vec<Value>>,
    dynamic: Option<std::sync::Arc<dyn Fn(&str) -> Value + Send + Sync>>,
    bound: Mutex<Option<u64>>,
    bind_count: std::sync::Arc<std::sync::atomic::AtomicU64>,
    complete_count: std::sync::Arc<std::sync::atomic::AtomicU64>,
    reset_count: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl Clone for MockBackend {
    fn clone(&self) -> Self {
        Self {
            script: Mutex::new(self.script.lock().expect("mock script poisoned").clone()),
            dynamic: self.dynamic.clone(),
            bound: Mutex::new(*self.bound.lock().expect("bound poisoned")),
            bind_count: self.bind_count.clone(),
            complete_count: self.complete_count.clone(),
            reset_count: self.reset_count.clone(),
        }
    }
}

impl std::fmt::Debug for MockBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockBackend").finish_non_exhaustive()
    }
}

impl MockBackend {
    /// 用一串信封 JSON 建立脚本后端（按调用顺序回放，耗尽后返回 `respond`）。
    pub fn new(script: Vec<Value>) -> Self {
        Self {
            script: Mutex::new(script),
            dynamic: None,
            bound: Mutex::new(None),
            bind_count: Default::default(),
            complete_count: Default::default(),
            reset_count: Default::default(),
        }
    }

    /// 用闭包动态生成信封（可依据输入文本决定输出）。
    pub fn dynamic(
        f: impl Fn(&str) -> Value + Send + Sync + 'static,
    ) -> Self {
        Self {
            script: Mutex::new(Vec::new()),
            dynamic: Some(std::sync::Arc::new(f)),
            bound: Mutex::new(None),
            bind_count: Default::default(),
            complete_count: Default::default(),
            reset_count: Default::default(),
        }
    }

    /// 追加一条脚本信封。
    pub fn push(&self, envelope: Value) {
        self.script
            .lock()
            .expect("mock script poisoned")
            .push(envelope);
    }

    /// 统计：bind 次数（测试重绑语义）。
    pub fn bind_count(&self) -> u64 {
        self.bind_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 统计：complete 次数（测试轮次推进）。
    pub fn complete_count(&self) -> u64 {
        self.complete_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// 统计：reset 次数。
    pub fn reset_count(&self) -> u64 {
        self.reset_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl NeedleBackend for MockBackend {
    fn bind(
        &self,
        signature: u64,
        _system: &str,
        _tools_json: &str,
        _tool_index: Option<&std::path::Path>,
    ) -> Result<(), NeedleError> {
        let mut bound = self.bound.lock().expect("bound poisoned");
        if *bound != Some(signature) {
            self.bind_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *bound = Some(signature);
        }
        Ok(())
    }

    fn complete(
        &self,
        input: &str,
        _max_new_tokens: u32,
        _buffer: &mut [u8],
    ) -> Result<NeedleResponse, NeedleError> {
        self.complete_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let value = if let Some(dynamic) = &self.dynamic {
            dynamic(input)
        } else {
            let mut script = self.script.lock().expect("mock script poisoned");
            if script.is_empty() {
                serde_json::json!({ "type": "respond", "success": true })
            } else {
                script.remove(0)
            }
        };
        let bytes = serde_json::to_vec(&value)
            .expect("mock envelope must serialize");
        NeedleResponse::parse(&bytes)
    }

    fn reset(&self) {
        self.reset_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut bound) = self.bound.lock() {
            *bound = None;
        }
    }
}
