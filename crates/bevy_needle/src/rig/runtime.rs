//! R2 运行时：懒创建 tokio runtime（规格 §3.5 / I22）。
//!
//! 红线 I1/I22：**主线程永不 block_on**；能 `block_on()` 的类型只存在于
//! worker 侧。本资源只做**懒创建与复用**：
//! - 第一次真正需要异步 IO 时才建 `Builder::new_multi_thread().enable_all()`；
//! - [`RigRuntime::with_runtime`] 让宿主复用已有 runtime（规格 §6.3）；
//! - 构造 client 前必须 enter（否则 client 绑错 reactor）——封装成
//!   [`RigRuntime::with_enter`] 作用域式 API。
//!
//! Drop 纪律：模型加载一旦进入 blocking pool 就不可取消（candle 事实），
//! 因此升级路径上**禁止**触发模型加载——那是 startup 的事（规格 §6.4 #2）。

use std::sync::Arc;

/// tokio 运行时持有者（`rig` feature 内部使用；worker 侧专属）。
#[derive(Clone)]
pub struct RigRuntime {
    inner: RuntimeInner,
}

#[derive(Clone)]
enum RuntimeInner {
    /// 懒创建：首次 enter/block_on 时构建。
    Lazy(Arc<OnceRuntime>),
    /// 宿主复用：调用方保证 runtime 存活期覆盖本资源。
    Shared(Arc<tokio::runtime::Runtime>),
}

struct OnceRuntime {
    lock: std::sync::Mutex<Option<Arc<tokio::runtime::Runtime>>>,
}

impl OnceRuntime {
    fn new() -> Self {
        Self {
            lock: std::sync::Mutex::new(None),
        }
    }

    fn get_or_init(&self) -> Result<Arc<tokio::runtime::Runtime>, String> {
        let mut guard = self
            .lock
            .lock()
            .map_err(|_| "RigRuntime lock poisoned".to_string())?;
        if guard.is_none() {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|err| format!("tokio runtime build failed: {err}"))?;
            *guard = Some(Arc::new(rt));
        }
        Ok(guard.as_ref().expect("just initialized").clone())
    }
}

impl std::fmt::Debug for RigRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            RuntimeInner::Lazy(_) => f.write_str("RigRuntime(lazy)"),
            RuntimeInner::Shared(_) => f.write_str("RigRuntime(shared)"),
        }
    }
}

impl RigRuntime {
    /// 默认：懒创建（第一次使用才构建 tokio runtime）。
    pub fn lazy() -> Self {
        Self {
            inner: RuntimeInner::Lazy(Arc::new(OnceRuntime::new())),
        }
    }

    /// 复用宿主已有 runtime（不破坏宿主的运行时选择）。
    pub fn with_runtime(rt: Arc<tokio::runtime::Runtime>) -> Self {
        Self {
            inner: RuntimeInner::Shared(rt),
        }
    }

    /// 在运行时上下文内执行闭包（构造 reqwest client 等需要 reactor 的对象时用；
    /// guard 借 runtime 无法装进结构体跨方法逃逸，故暴露为作用域式 API）。
    ///
    /// **只允许在 worker 线程调用**（I1/I22，调用方结构保证）。
    ///
    /// # Errors
    /// 懒创建失败（runtime 构建错误）。
    pub fn with_enter<R>(&self, f: impl FnOnce() -> R) -> Result<R, String> {
        match &self.inner {
            RuntimeInner::Shared(rt) => {
                let _guard = rt.enter();
                Ok(f())
            }
            RuntimeInner::Lazy(once) => {
                let rt = once.get_or_init()?;
                let _guard = rt.enter();
                Ok(f())
            }
        }
    }

    /// 在 worker 线程上 block 一个 future（I1/I22 的唯一合法入口：
    /// 只应由 worker 线程调用，ECS 主线程禁止）。
    ///
    /// # Errors
    /// 懒创建失败。
    pub fn block_on<F: std::future::Future>(&self, fut: F) -> Result<F::Output, String> {
        match &self.inner {
            RuntimeInner::Shared(rt) => Ok(rt.block_on(fut)),
            RuntimeInner::Lazy(once) => {
                let rt = once.get_or_init()?;
                Ok(rt.block_on(fut))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lazy_runtime_initializes_once() {
        let runtime = RigRuntime::lazy();
        runtime.with_enter(|| {}).expect("lazy init");
        let once = match &runtime.inner {
            RuntimeInner::Lazy(once) => once,
            _ => unreachable!(),
        };
        let first = once.get_or_init().expect("init");
        let second = once.get_or_init().expect("reuse");
        assert!(Arc::ptr_eq(&first, &second));
    }

    #[test]
    fn shared_runtime_is_used() {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("build"),
        );
        let runtime = RigRuntime::with_runtime(rt.clone());
        assert_eq!(runtime.block_on(async { 42u32 }).unwrap(), 42);
        drop(runtime);
        assert_eq!(rt.block_on(async { 7u32 }), 7);
    }
}
