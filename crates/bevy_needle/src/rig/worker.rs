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
//! 绝不静默丢弃。等待是**事件驱动**的：future 在 `Pending` 前向槽注册 waker
//! （[`futures::task::AtomicWaker`]），worker 落回时精确唤醒一次——主线程
//! poll 永不阻塞、永不自旋、永不 spawn 线程（I1/I22）。

use std::future::Future;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use futures::task::AtomicWaker;

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
///
/// 唤醒是事件驱动的（升级计划 §27.5 ①）：[`futures::task::AtomicWaker`]
/// 持有等待方注册的 waker，worker 落回时精确唤醒——不再依赖"等待方反复
/// 轮询/重试"来推进 future。
pub(crate) struct WaitSlot {
    state: Mutex<SlotState>,
    waker: AtomicWaker,
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
            waker: AtomicWaker::new(),
        }
    }

    /// worker 落回（非阻塞；成功与失败同样落回，I27）。
    ///
    /// 状态置位后（锁外）唤醒注册的 waker——`AtomicWaker::wake` 可与
    /// `register` 并发，唤醒竞态由 poll 端的"注册后复查"防线兜住。
    fn deliver(self: &Arc<Self>, outcome: Result<NeedleResponse, WorkerError>) {
        {
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
        }
        self.waker.wake();
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
/// poll 语义（AtomicWaker 标准两步式，杜绝丢唤醒）：
/// 1. 先非阻塞取一次——worker 可能早已落回；
/// 2. 未落回则注册 waker，**再取一次**（注册与 deliver 并发时的竞态防线：
///    若 deliver 恰在注册前完成，原子唤醒会丢失，复查兜住这一窗口）；
/// 3. 仍未落回 → `Pending`，推进权全在 worker 的下一次 wake。
/// Drop 时把槽标记弃用：worker 之后落回的结果被丢弃（无人读，不悬挂）。
pub struct WaitFuture {
    slot: Arc<WaitSlot>,
}

impl Future for WaitFuture {
    type Output = Result<NeedleResponse, WorkerError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        if let Some(outcome) = this.slot.try_take() {
            return Poll::Ready(outcome);
        }
        this.slot.waker.register(cx.waker());
        // 注册后复查：deliver 可能在注册生效前已原子唤醒（丢失的是"唤醒"
        // 而不是帧），复查保证这一窗口不产生永久 Pending。
        if let Some(outcome) = this.slot.try_take() {
            return Poll::Ready(outcome);
        }
        Poll::Pending
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
/// 引擎时，bind 互相覆盖。多会话需求（升级计划 §29.3/§13）下宿主应给每个
/// Agent 独立 worker 进程（上游 issue #94 的官方策略）；本 crate 当前提供的
/// 进程内形态按 §13 第一版约定：**一个 Rig Agent = 一个 Needle3Model = 一个
/// worker 实例，且宿主保证不并发驱动多个实例**（Rig 调度层串行保证）。
///
/// ## 生命周期（§29.3 任务 B：明确的 shutdown / join）
///
/// `stop accepting → drain / discard → join worker`：
/// - [`Needle3Worker::shutdown`] 幂等——第一次调用取走发送端（**停止收新
///   作业**），随后 [`Needle3Worker::join`] 等待线程收尽队列并退出；
/// - 队列里已排队的作业**照常执行**（drain）；其后退出的作业结果：
///   等待方在 → 交付，等待方已 drop（`Abandoned`）→ 丢弃（I27：无静默
///   失联，回执要么交付要么显式丢弃）；
/// - shutdown 之后 [`Needle3Worker::submit`] 立即以
///   `WorkerError::Transport("worker shut down")` 回执（不悬挂、不排队）。
///
/// **Drop 只停收不 join**（避免宿主丢弃共享句柄时阻塞任意线程）——"何时
/// 确信线程真的结束"由显式 `shutdown`/`join` 回答（§29.3 禁止只依赖
/// `drop(Sender)` + 丢弃 JoinHandle）。
pub struct Needle3Worker {
    /// 发送端（shutdown 时取走 = 停止收新作业）。
    jobs: Mutex<Option<Sender<Job>>>,
    /// 线程句柄（shutdown 时取走并 join）。
    worker: Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct Job {
    payload: NeedlePayload,
    slot: Arc<WaitSlot>,
}

impl Drop for Needle3Worker {
    /// 只停收（取走发送端），**不 join**——共享句柄的最后一个引用可能
    /// 落在任何线程；join 的阻塞语义由显式 [`Self::shutdown`] 承担
    /// （§29.3："宿主何时能确信线程真的结束"）。
    fn drop(&mut self) {
        if let Ok(mut guard) = self.jobs.lock() {
            guard.take();
        }
    }
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
            jobs: Mutex::new(Some(jobs)),
            worker: Mutex::new(worker),
        }
    }

    /// 提交一次解码（立即返回；I1）。
    ///
    /// shutdown 之后：**立即**回执 `Transport("worker shut down")`（不排队
    /// 不悬挂，I27 / §29.3「不接新作业」）。
    pub fn submit(&self, payload: NeedlePayload) -> Submitted {
        let slot = Arc::new(WaitSlot::new());
        let guard = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match guard.as_ref() {
            // 正常路径：入队成功即返回等待句柄。
            Some(jobs)
                if jobs
                    .send(Job {
                        payload,
                        slot: Arc::clone(&slot),
                    })
                    .is_ok() => {}
            // 已 shutdown 或通道断开：立即回执（I27，不排队不悬挂）。
            _ => {
                drop(guard);
                slot.deliver(Err(WorkerError::Transport(
                    "needle worker shut down".into(),
                )));
            }
        }
        Submitted {
            ticket: Ticket(0),
            wait: WaitFuture { slot },
        }
    }

    /// 生命周期收尾（§29.3 任务 B；幂等）：
    /// **stop accepting → drain / discard → join worker**。
    ///
    /// 第一次调用：取走发送端（之后 [`Self::submit`] 立即以 shutdown 错误
    /// 回执），join worker——队列剩余作业照常执行（drain），其后退出。
    /// 阻塞语义：join 等待**在途解码天然完成**（`needle_complete` 不可中止，
    /// candle 事实，规格 §7）——宿主需在调用处容忍该时长。
    pub fn shutdown(&self) -> Result<(), WorkerError> {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        self.join()
    }

    /// join worker 线程（幂等；shutdown 内部调用；也可单独使用——
    /// 取走发送端后再 join 保证可退出）。
    ///
    /// 注意：单独 join 而发送端仍在的场景由返回值表达——发送端尚未取走
    /// 时返回 `Err(WorkerError::Transport)` 且**不阻塞**（要求宿主先
    /// shutdown 再 join 即可确信线程结束）。
    pub fn join(&self) -> Result<(), WorkerError> {
        let handle = {
            let mut guard = self
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.take()
        };
        let Some(handle) = handle else {
            return Ok(()); // 已 join：幂等。
        };
        let jobs_live = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some();
        if jobs_live {
            // 发送端还活着的场景不 join（否则永远等不到队列耗尽）；放回
            // 句柄，保持幂等语义 TObject。
            *self
                .worker
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(handle);
            return Err(WorkerError::Transport(
                "needle worker join before shutdown (sender still live)".into(),
            ));
        }
        match handle.join() {
            Ok(()) => Ok(()),
            Err(_) => Err(WorkerError::Transport(
                "needle worker thread panicked before join".into(),
            )),
        }
    }

    /// 是否已 shutdown（停止收新作业）。
    pub fn is_shut_down(&self) -> bool {
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_none()
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

/// `NeedleError` → worker 错误文本（错误分类投影；本 crate 自持，
/// 不借 legacy `needle_runtime` 的文案面——⑦ 冻结纪律）。
fn describe(err: &NeedleError) -> String {
    error_text(err)
}

/// [`NeedleError`] 的用户面投影（worker 侧独立持有；不借 legacy 文案面——
/// ⑦ 冻结纪律：新代码不依赖 legacy）。
fn error_text(err: &NeedleError) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Waker;
    use std::time::Duration;

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

    /// 阻塞型后端：`complete()` 挂起直到测试放行（验证事件驱动唤醒）。
    struct GateBackend {
        release_rx: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl NeedleBackend for GateBackend {
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
            _input: &str,
            _max_new_tokens: u32,
            _buffer: &mut [u8],
        ) -> Result<NeedleResponse, NeedleError> {
            // 挂起 worker 线程直到放行（通道关闭 = 测试退出，同样合法回执）。
            let released = self
                .release_rx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .recv()
                .is_ok();
            let body = serde_json::to_vec(&json!({
                "type": "respond",
                "success": released,
                "function_calls": [],
                "reasoning": "released",
            }))
            .expect("serialize");
            NeedleResponse::parse(&body)
        }

        fn reset(&self) {}
    }

    /// 可观测的 waker：wake 时置位（`ArcWake`，AtomicWaker 的标准被唤醒方）。
    #[derive(Default)]
    struct WakeFlag(AtomicBool);

    impl futures::task::ArcWake for WakeFlag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }

        fn wake_by_ref(arc_self: &Arc<Self>) {
            arc_self.0.store(true, Ordering::SeqCst);
        }
    }

    /// ① 的核心验收：worker 落回必须**事件驱动**地唤醒已注册 waker——
    /// 中间没有任何 re-poll（旧实现靠每次 poll spawn 睡眠线程推进，
    /// 执行器若不再重试轮询即永久 Pending：这就是规格点名的挂死隐患）。
    #[test]
    fn deliver_wakes_registered_waker_without_repoll() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let worker = Needle3Worker::new(Arc::new(GateBackend {
            release_rx: Mutex::new(release_rx),
        }));
        let submitted = worker.submit(NeedlePayload {
            input: "gated".into(),
            max_new_tokens: 8,
            session: None,
        });

        let flag = Arc::new(WakeFlag::default());
        let waker = futures::task::waker(flag.clone());
        let mut cx = Context::from_waker(&waker);
        let mut fut = std::pin::pin!(submitted.wait);

        // 1. 首次 poll：帧未落回 → Pending + 注册 waker。
        assert!(
            matches!(fut.as_mut().poll(&mut cx), Poll::Pending),
            "门控未放行前 future 必须保持 Pending"
        );
        assert!(
            !flag.0.load(Ordering::SeqCst),
            "帧未落回时不得唤醒（假唤醒会让执行器空转）"
        );

        // 2. 放行 worker → deliver → 原子唤醒。期间**零 re-poll**。
        let _ = release_tx.send(());
        let woken = {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if flag.0.load(Ordering::SeqCst) {
                    break true;
                }
                if std::time::Instant::now() > deadline {
                    break false;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        assert!(
            woken,
            "worker 落回必须唤醒注册的 waker（AtomicWaker 事件驱动，非重试轮询）"
        );

        // 3. 被唤醒后的下一次 poll 取走帧。
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(Ok(response)) => {
                assert_eq!(response.reasoning.as_deref(), Some("released"));
            }
            other => panic!("唤醒后 poll 应取走帧，得到 {other:?}"),
        }
    }

    /// 丢唤醒防线：落回发生在"注册生效前"窗口时，注册后的复查必须兜住。
    #[test]
    fn late_poll_still_observes_frame_delivered_before_registration() {
        let worker = Needle3Worker::new(Arc::new(EchoBackend));
        let submitted = worker.submit(NeedlePayload {
            input: "already-done".into(),
            max_new_tokens: 8,
            session: None,
        });
        // 自旋等待 worker 落回（绝不 poll future），再首 poll——
        // 该 poll 的 try_take 直接命中帧（寄存前已 Ready 的路径）。
        let waker = Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut fut = std::pin::pin!(submitted.wait);
        let mut outcome = None;
        for _ in 0..10_000 {
            // noop waker poll：落回先于注册时，首次 try_take 即命中
            // （"注册后复查"防线的另一半——帧先到、waker 后注册）。
            if let Poll::Ready(value) = fut.as_mut().poll(&mut cx) {
                outcome = Some(value.expect("echo backend succeeds"));
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        let outcome = outcome.expect("frame should have been observed");
        assert_eq!(outcome.reasoning.as_deref(), Some("already-done"));
    }

    /// ── §29.3 任务 B 验收组 ──

    /// shutdown 幂等 + 停止收新作业 + join 确认。
    #[test]
    fn shutdown_is_idempotent_and_stops_accepting() {
        let worker = Needle3Worker::new(Arc::new(EchoBackend));
        // shutdown 前作业照常。
        let pre = worker.submit(NeedlePayload {
            input: "pre".into(),
            max_new_tokens: 8,
            session: None,
        });
        futures_now(pre.wait).expect("pre-shutdown job completes");

        let first = worker.shutdown();
        first.expect("first shutdown joins cleanly");
        assert!(worker.is_shut_down(), "shutdown 后不再收新作业");

        // 幂等：再次 shutdown 无副作用。
        worker.shutdown().expect("second shutdown is a no-op");

        // shutdown 后 submit：立即获得 shutdown 回执（不排队不悬挂）。
        let post = worker.submit(NeedlePayload {
            input: "post".into(),
            max_new_tokens: 8,
            session: None,
        });
        let receipt = futures_now(post.wait);
        assert!(
            matches!(&receipt, Err(WorkerError::Transport(msg)) if msg.contains("shut down")),
            "shutdown 后 submit 必须 Transport 错误回执：{receipt:?}"
        );
    }

    /// drain 语义：shutdown 前已排队的作业**照常执行并交付**（不是丢队列）。
    #[test]
    fn shutdown_drains_queued_jobs() {
        let worker = Needle3Worker::new(Arc::new(EchoBackend));
        let jobs: Vec<Submitted> = (0..3)
            .map(|n| {
                worker.submit(NeedlePayload {
                    input: format!("job-{n}"),
                    max_new_tokens: 8,
                    session: None,
                })
            })
            .collect();

        worker.shutdown().expect("shutdown with queued jobs drains");

        // 三个等待 future 全部交付（drain——结果没有被丢弃）。
        for (n, submitted) in jobs.into_iter().enumerate() {
            let response = futures_now(submitted.wait).expect("queued job delivered");
            assert_eq!(
                response.reasoning.as_deref(),
                Some(format!("job-{n}").as_str()),
                "queued job {n} must be drained, not discarded"
            );
        }
        assert!(worker.is_shut_down());
    }

    /// join-before-shutdown：发送端仍活时不 join（返回显式错误，不阻塞）。
    #[test]
    fn join_before_shutdown_refuses_without_blocking() {
        let worker = Needle3Worker::new(Arc::new(EchoBackend));
        let result = worker.join();
        assert!(
            matches!(&result, Err(WorkerError::Transport(msg)) if msg.contains("before shutdown")),
            "发送端仍活时 join 必须显式拒绝：{result:?}"
        );
        // 补做 shutdown → join 干净完成。
        worker.shutdown().expect("shutdown after refusal");
    }

    /// 关闭后的迟到结果语义（§29.3 最后一条）：shutdown 期间在途作业
    /// 照常完成（drain），交付给仍在等待的 future；等待方已丢（Abandoned）
    /// 则显式丢弃。run 层面的"复活防线"由 cancellation e2e 承担
    /// （rig_ecs_host::cancel_run_mid_flight_discards_late_completion）。
    #[test]
    fn shutdown_waits_for_in_flight_decode_and_delivers() {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        struct GateBackend {
            release_rx: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl NeedleBackend for GateBackend {
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
                _input: &str,
                _max_new_tokens: u32,
                _buffer: &mut [u8],
            ) -> Result<NeedleResponse, NeedleError> {
                let _ = self.release_rx.lock().expect("gate").recv();
                let body = serde_json::to_vec(&json!({
                    "type": "respond",
                    "success": true,
                    "function_calls": [],
                    "reasoning": "released during drain",
                }))
                .expect("serialize");
                NeedleResponse::parse(&body)
            }
            fn reset(&self) {}
        }
        let worker = Needle3Worker::new(Arc::new(GateBackend {
            release_rx: Mutex::new(release_rx),
        }));
        let submitted = worker.submit(NeedlePayload {
            input: "in-flight".into(),
            max_new_tokens: 8,
            session: None,
        });
        // 放行器：shutdown 的 join 会等在途解码完成——先定时放行。
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            let _ = release_tx.send(());
        });
        worker.shutdown().expect("shutdown drains in-flight job");
        let response = futures_now(submitted.wait).expect("in-flight job delivered");
        assert_eq!(response.reasoning.as_deref(), Some("released during drain"));
        let _ = releaser.join();
        assert!(worker.is_shut_down());
    }
}
