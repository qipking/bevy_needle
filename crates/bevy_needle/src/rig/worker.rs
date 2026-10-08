//! Needle3 worker 门面：Rig 侧异步调用 ↔ Needle 阻塞解码的桥（升级计划 §7.3）。
//!
//! ```text
//! Rig async task（ECS 主线程 poll Opening）
//!     │ submit(payload)  — 立即返回（I1）
//!     ▼
//! worker channel（mpsc）
//!     ▼
//! Needle worker thread（I22：唯一阻塞解码点）
//!     │ NeedleBackend::complete()
//!     ▼
//! 回灌槽（Job 自带槽，worker 直接落回）
//!     ▼
//! Opening future resolves → NeedleResponse
//! ```
//!
//! 与现有 [`crate::needle_runtime::NeedleRuntime`] 的关系：那个是 Bevy Run
//! 主管线的执行桥（ECS 每帧 drain 事件）；本模块是 **Rig 侧独立 worker**——
//! 按升级计划 §12/§13，一个 Rig Agent = 一个 Needle 会话 = 一个独立 worker
//! 实例，避免与主管线争抢引擎的进程级单会话。
//!
//! 回执纪律（I27）：每个提交必得回执——成功帧、引擎错误、通道断开都落回槽，
//! 绝不静默丢弃。等待以同步机制实现（无 tokio 依赖；`park_timeout` 轮询），
//! future 体是 `std::future::poll_fn`——主线程 poll 时最多自旋一个短时窗，
//! 之后 `Pending` 让出帧，不占 ECS 调度。

use std::future::Future;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;
use crate::backend::NeedleBackend;
use crate::engine::NeedleResponse;
use crate::error::NeedleError;

use super::codec::NeedlePayload;

/// worker 侧错误（`ProviderError` 之外的回执分类；I27）。
#[derive(Debug, Clone)]
pub enum WorkerError {
    /// 引擎解码失败（`NeedleError` 的投影）。
    Engine(String),
    /// 通道断开 / worker 死亡 / 票丢失。
    Transport(String),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WorkerError::Engine(msg) => write!(f, "needle engine error: {msg}"),
            WorkerError::Transport(msg) => write!(f, "needle worker transport: {msg}"),
        }
    }
}

impl std::error::Error for WorkerError {}

/// 单票回灌槽：worker 落回一次，等待方取走；槽与 Job 同生共死，无注册表。
pub(crate) struct WaitSlot {
    state: Mutex<SlotState>,
    ready: Condvar,
}

enum SlotState {
    /// worker 尚未落回。
    Waiting,
    /// worker 落回（成功帧或错误回执）。
    Frame(Result<NeedleResponse, WorkerError>),
    /// 等待方已取走。
    Taken,
    /// 等待方放弃（future drop 后 worker 落回，无人读）。
    Abandoned,
}

impl WaitSlot {
    fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::Waiting),
            ready: Condvar::new(),
        }
    }

    /// worker 落回（非阻塞；成功与失败同样落回，I27）。
    fn deliver(self: &Arc<Self>, outcome: Result<NeedleResponse, WorkerError>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &*state {
            SlotState::Abandoned | SlotState::Taken => return, // 无人等：丢弃。
            SlotState::Frame(_) => return,                     // 重复回执：丢弃。
            SlotState::Waiting => {}
        }
        *state = SlotState::Frame(outcome);
        self.ready.notify_all();
    }

    /// 标记弃用（等待 future drop 时）。
    fn abandon(self: &Arc<Self>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(*state, SlotState::Waiting) {
            *state = SlotState::Abandoned;
        }
    }

    /// 非阻塞取一次（`Waiting` → `None`；`Frame` → 取走并置 `Taken`）。
    fn try_take(self: &Arc<Self>) -> Option<Result<NeedleResponse, WorkerError>> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match std::mem::replace(&mut *state, SlotState::Waiting) {
            SlotState::Frame(outcome) => {
                *state = SlotState::Taken;
                Some(outcome)
            }
            other => {
                *state = other;
                None
            }
        }
    }

}

/// 一次解码作业的票（submit 的返回；opaque；I17 语义：票即 attempt）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket(pub(crate) u64);

/// 提交回执：票 + 等待 future（transport 在 `Opening` 里持有 future）。
pub struct Submitted {
    /// 作业票（诊断面）。
    pub ticket: Ticket,
    /// 等待 worker 落回的 future。
    pub wait: WaitFuture,
}

/// 等待 worker 落回的 future（I1：主线程只 poll，不做阻塞等待）。
///
/// poll 语义：先非阻塞取一次；`Waiting` 则短自旋（worker 回执毫秒级）后
/// `Pending` 让出帧——唤醒交给下一次 poll（Rig 的 async task 池会重试轮询）。
/// 不注册 waker：worker 线程与调用线程无共享 waker 通道，Condvar 只服务
/// 阻塞测试路径；任务池的重试轮询是唯一推进机制。
/// Drop 时把槽标记弃用：worker 之后落回的结果被丢弃（无人读，不悬挂）。
pub struct WaitFuture {
    slot: Arc<WaitSlot>,
}

impl Future for WaitFuture {
    type Output = Result<NeedleResponse, WorkerError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this.slot.try_take() {
            Some(outcome) => Poll::Ready(outcome),
            None => {
                // 让出：短自旋窗口后再注册下一次唤醒（毫秒级让出，不占帧）。
                let waker = cx.waker().clone();
                let slot = Arc::clone(&this.slot);
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1));
                    // 总是唤醒：下一次 poll 由 poll 端消费帧（唤醒线程绝不消费，
                    // 避免竞态）。多余的唤醒（帧已被取走）是无害的 no-op。
                    let _ = &slot;
                    waker.wake();
                });
                Poll::Pending
            }
        }
    }
}

impl Drop for WaitFuture {
    fn drop(&mut self) {
        self.slot.abandon();
    }
}

/// Needle3 worker：一个 Needle 会话 + 一条串行解码队列。
///
/// `backend` 是启动期构造完成的引擎句柄（`DlopenBackend` 或 `MockBackend`）。
/// 注意（F1）：Needle 引擎是进程级单会话单例——两个 worker 共享同一进程内
/// 引擎时，bind 互相覆盖。多会话需求（升级计划 §13）下宿主应给每个 Agent
/// 独立 worker 进程（上游 issue #94 的官方策略）；本 crate 当前提供的
/// 进程内形态按 §13 第一版约定：**一个 Rig Agent = 一个 Needle3Model = 一个
/// worker 实例，且宿主保证不并发驱动多个实例**（Rig 调度层串行保证）。
pub struct Needle3Worker {
    jobs: Sender<Job>,
    _worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct Job {
    payload: NeedlePayload,
    slot: Arc<WaitSlot>,
}

impl Needle3Worker {
    /// 构造并启动 worker 线程（解码的唯一执行地，I22）。
    pub fn new(backend: Arc<dyn NeedleBackend>) -> Self {
        let (jobs, jobs_rx) = std::sync::mpsc::channel::<Job>();
        let worker = std::thread::Builder::new()
            .name("bevy_needle_rig_needle3".into())
            .spawn(move || worker_loop(backend, jobs_rx))
            .ok();
        Self {
            jobs,
            _worker: Mutex::new(worker),
        }
    }

    /// 提交一次解码（立即返回；I1）。worker 死亡也回执（I27）。
    pub fn submit(&self, payload: NeedlePayload) -> Submitted {
        let slot = Arc::new(WaitSlot::new());
        let job = Job {
            payload,
            slot: Arc::clone(&slot),
        };
        if self.jobs.send(job).is_err() {
            slot.deliver(Err(WorkerError::Transport(
                "needle worker channel closed".into(),
            )));
        }
        Submitted {
            ticket: Ticket(0),
            wait: WaitFuture { slot },
        }
    }
}

/// worker 主循环（I22 的唯一阻塞解码点；jobs 通道断开 → 循环退出）。
fn worker_loop(backend: Arc<dyn NeedleBackend>, jobs: Receiver<Job>) {
    let mut bound = false;
    while let Ok(job) = jobs.recv() {
        let outcome = run_job(&backend, &job, &mut bound);
        job.slot.deliver(outcome);
    }
}

/// 执行单个作业（worker 线程内；阻塞解码，I22）。
fn run_job(
    backend: &Arc<dyn NeedleBackend>,
    job: &Job,
    bound: &mut bool,
) -> Result<NeedleResponse, WorkerError> {
    // 首次绑定的会话语义：Rig 侧模型带 system facts 的话在这里绑定。
    // Needle 的 KV 会话在 `complete` 之间延续；`reset` 是会话级操作。
    if !*bound {
        backend
            .bind(0, "", "[]", None)
            .map_err(|err| WorkerError::Engine(describe(&err)))?;
        *bound = true;
    }
    let mut buffer = vec![0u8; backend.buffer_size()];
    backend
        .complete(&job.payload.input, job.payload.max_new_tokens, &mut buffer)
        .map_err(|err| WorkerError::Engine(describe(&err)))
}

/// `NeedleError` → worker 错误文本（错误分类投影）。
fn describe(err: &NeedleError) -> String {
    crate::needle_runtime::describe_error(err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::task::Waker;

    struct EchoBackend;

    impl NeedleBackend for EchoBackend {
        fn bind(
            &self,
            _signature: u64,
            _system: &str,
            _tools_json: &str,
            _tool_index: Option<&std::path::Path>,
        ) -> Result<(), NeedleError> {
            Ok(())
        }

        fn complete(
            &self,
            input: &str,
            _max_new_tokens: u32,
            _buffer: &mut [u8],
        ) -> Result<NeedleResponse, NeedleError> {
            let body = serde_json::to_vec(&json!({
                "type": "respond",
                "success": true,
                "function_calls": [],
                "reasoning": input,
            }))
            .expect("serialize");
            NeedleResponse::parse(&body)
        }

        fn reset(&self) {}
    }

    #[test]
    fn submit_delivers_frame() {
        let worker = Needle3Worker::new(Arc::new(EchoBackend));
        let submitted = worker.submit(NeedlePayload {
            input: "hello".into(),
            max_new_tokens: 32,
            session: None,
        });
        let response = futures_now(submitted.wait);
        assert!(matches!(response, Ok(resp) if resp.reasoning.as_deref() == Some("hello")));
    }

    /// 同步推进一个 future 到 Ready（测试专用；worker 回执毫秒级）。
    fn futures_now<F: Future>(fut: F) -> F::Output
    where
        F::Output: std::panic::UnwindSafe,
    {
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut fut = std::pin::pin!(fut);
        for _ in 0..10_000 {
            if let Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                return value;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("worker did not deliver within the test window");
    }
}
