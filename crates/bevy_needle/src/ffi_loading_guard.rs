//! `libloading::Library` 的轻量包装。
//!
//! 独立成模块是为了把 `unsafe` 集中在 [`crate::ffi`]：这里提供
//! “按名字解析符号并得到 `*const ()`”的唯一通道，且库句柄在 [`LibraryGuard`]
//! 存活期内永不卸载（引擎是进程级单例，卸载本就不该发生）。

#![allow(unsafe_code)]

use std::path::Path;

use crate::error::NeedleError;

/// dlopen 句柄守卫。Clone 被刻意禁止——句柄与函数指针的生命周期必须一对一。
pub struct LibraryGuard {
    lib: libloading::Library,
}

impl std::fmt::Debug for LibraryGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryGuard").finish_non_exhaustive()
    }
}

impl LibraryGuard {
    /// 打开动态库。
    pub fn new(path: &Path) -> Result<Self, NeedleError> {
        let lib = unsafe { libloading::Library::new(path) }.map_err(|source| {
            NeedleError::LibraryLoad {
                path: path.to_path_buf(),
                reason: source.to_string(),
            }
        })?;
        Ok(Self { lib })
    }

    /// 解析符号为裸指针（调用方负责 transmute 到正确签名，见 [`crate::ffi`]）。
    pub fn symbol(&self, name: &'static str) -> Result<*const (), NeedleError> {
        let sym: libloading::Symbol<*const ()> = unsafe { self.lib.get(name.as_bytes()) }
            .map_err(|source| NeedleError::MissingSymbol {
                symbol: name,
                reason: source.to_string(),
            })?;
        Ok(*sym)
    }
}
