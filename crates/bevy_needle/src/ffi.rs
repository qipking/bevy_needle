//! 唯一允许 `unsafe` 的模块：needle2 引擎的四个 C 入口。
#![allow(unsafe_code)]
//!
//! C 契约与上游 Python 绑定（`needle/__init__.py`）一一对应：
//!
//! ```c
//! int  needle_init(const char *system, const char *tools_json, const char *tool_index_path);
//! int  needle_complete(const char *text, int max_new_tokens, char *out, int out_len);
//! void needle_reset(void);
//! int  needle_load(const char *blob, unsigned long long len);
//! ```
//!
//! 约定：返回值 `< 0` 失败；`needle_complete` 把以 NUL 结尾的 JSON 信封写进 out。
//!
//! 安全性说明（`#![deny(unsafe_code)]` 下的本地豁免）：
//! * 所有裸指针都来自本模块内 newly-created 的 `CString` / `Vec`，
//!   生命周期不超出调用表达式；
//! * `needle_complete` 写入的 buffer 由调用方持有（`NeedleEngineState::buffer`），
//!   读取前先按 NUL 截断再转 `&str`；
//! * 符号在 [`FfiEngine::open`] 时一次性解析并**转成裸函数指针**保存，
//!   避免每次调用走 `libloading::Symbol` 借用；`Library` 与指针同结构体共存亡，
//!   dlopen 的句柄在进程存活期内不卸载。
//!
//! SAFETY（符号指针化）：`libloading::Symbol::into_raw` 返回的指针与 `Library`
//! 同生命周期；这里转成 `'static` fn 指针的前提是 `Library` 句柄与本结构体共存
//! （`_lib` 字段）且永不 drop——对 dlopen 的引擎而言成立（进程级单例）。

use std::ffi::{c_char, c_int, c_ulonglong, CString};
use std::path::Path;

use crate::error::NeedleError;

type RawInit = unsafe extern "C" fn(*const c_char, *const c_char, *const c_char) -> c_int;
type RawComplete = unsafe extern "C" fn(*const c_char, c_int, *mut c_char, c_int) -> c_int;
type RawReset = unsafe extern "C" fn();
type RawLoad = unsafe extern "C" fn(*const c_char, c_ulonglong) -> c_int;

/// 已打开的引擎库与解析好的入口点。
pub struct FfiEngine {
    /// 保持 dlopen 句柄存活；见类型级 SAFETY 注释。
    _lib: crate::ffi_loading_guard::LibraryGuard,
    init: RawInit,
    complete: RawComplete,
    reset: RawReset,
    load: RawLoad,
}

impl std::fmt::Debug for FfiEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiEngine").finish_non_exhaustive()
    }
}

impl FfiEngine {
    /// dlopen 引擎并解析全部四个符号；缺符号立刻报错（版本错配会响亮地失败）。
    pub fn open(path: &Path) -> Result<Self, NeedleError> {
        let lib = crate::ffi_loading_guard::LibraryGuard::new(path)?;

        // SAFETY: 符号指针来自已加载的库（句柄存于 self._lib，永不卸载），
        // transmute 到的签名与引擎 C ABI 逐一核对（见模块级文档）。
        let init: RawInit = unsafe { std::mem::transmute(lib.symbol("needle_init")?) };
        let complete: RawComplete = unsafe { std::mem::transmute(lib.symbol("needle_complete")?) };
        let reset: RawReset = unsafe { std::mem::transmute(lib.symbol("needle_reset")?) };
        let load: RawLoad = unsafe { std::mem::transmute(lib.symbol("needle_load")?) };

        Ok(Self {
            _lib: lib,
            init,
            complete,
            reset,
            load,
        })
    }

    /// `needle_init`：绑定 system facts + 工具集（JSON 数组）+ 可选工具索引。
    pub fn init(
        &self,
        system: &str,
        tools_json: &str,
        tool_index: Option<&str>,
    ) -> Result<(), NeedleError> {
        let system = cstring("system", system)?;
        let tools = cstring("tools_json", tools_json)?;
        let index = match tool_index {
            Some(p) => Some(cstring("tool_index_path", p)?),
            None => None,
        };
        // SAFETY: 所有指针都指向本函数内创建的 C 字符串，调用期间有效。
        let rc = unsafe {
            (self.init)(
                system.as_ptr(),
                tools.as_ptr(),
                index.as_ref().map_or(std::ptr::null(), |c| c.as_ptr()),
            )
        };
        if rc < 0 {
            return Err(NeedleError::InitFailed(rc));
        }
        Ok(())
    }

    /// `needle_complete`：阻塞解码一轮，把 NUL 结尾的 JSON 信封写进 `out`，
    /// 返回有效字节数。
    pub fn complete(&self, text: &str, max_new_tokens: u32, out: &mut [u8]) -> Result<usize, NeedleError> {
        let text = cstring("text", text)?;
        // SAFETY: out 由调用方持有；引擎只在调用期间写入。
        let rc = unsafe {
            (self.complete)(
                text.as_ptr(),
                max_new_tokens as c_int,
                out.as_mut_ptr() as *mut c_char,
                out.len() as c_int,
            )
        };
        if rc < 0 {
            return Err(NeedleError::CompleteFailed(rc));
        }
        let end = out
            .iter()
            .position(|&b| b == 0)
            .ok_or(NeedleError::BufferNotTerminated)?;
        Ok(end)
    }

    /// `needle_reset`：回退当前会话（工具集保留）。
    pub fn reset(&self) {
        // SAFETY: 无参数无返回；引擎要求在 init 之后调用。
        unsafe { (self.reset)() };
    }

    /// `needle_load`：加载调优 `.cact` 权重（进程内一次性，不可卸载）。
    pub fn load_weights(&self, blob: &[u8]) -> Result<(), NeedleError> {
        // SAFETY: blob 指针在调用期间有效。
        let rc = unsafe { (self.load)(blob.as_ptr() as *const c_char, blob.len() as c_ulonglong) };
        if rc != 0 {
            return Err(NeedleError::LoadWeightsFailed(rc));
        }
        Ok(())
    }
}

fn cstring(field: &'static str, value: &str) -> Result<CString, NeedleError> {
    CString::new(value.as_bytes()).map_err(|_| NeedleError::InteriorNul { field })
}
