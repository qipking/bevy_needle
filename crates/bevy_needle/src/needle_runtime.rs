//! 引擎执行桥：阻塞解码在独立工作线程上串行执行，ECS 每帧只做提交与收割。
//!
//! 所有 run 的 turn 请求都在同一条工作线程上排队（引擎是进程级单会话）；
//! 每个 [`TurnJob`] 携带 agent 快照签名，签名变化时工作线程自动重绑。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use bevy_ecs::prelude::Entity;
use bevy_ecs::resource::Resource;

use crate::backend::NeedleBackend;
use crate::engine::NeedleResponse;
use crate::error::NeedleError;

/// 一轮解码请求（字段用 `Arc<str>` 以避免每轮克隆工具集字符串）。
#[derive(Clone, Debug)]
pub struct TurnJob {
    /// 目标 run 实体。
    pub run: Entity,
    /// agent 快照签名（变化即重绑）。
    pub signature: u64,
    /// system facts。
    pub system: Arc<str>,
    /// 工具集 JSON（由 AgentToolIndex 编译）。
    pub tools_json: Arc<str>,
    /// 工具索引缓存路径（大于 5 个工具时加速检索头）。
    pub tool_index: Option<PathBuf>,
    /// 本轮输入（用户文本或工具结果 JSON）。
    pub input: Arc<str>,
    /// 本轮最大生成 token。
    pub max_new_tokens: u32,
    /// 输出缓冲大小。
    pub buffer_size: usize,
}

/// 发给工作线程的作业。
pub enum RuntimeJob {
    /// 执行一轮解码。
    Turn(TurnJob),
    /// 重置引擎会话。
    Reset,
}

/// 工作线程发回的事件。
pub enum RuntimeEvent {
    /// 一轮解码完成。
    TurnCompleted {
        /// 目标 run。
        run: Entity,
        /// 引擎信封。
        response: NeedleResponse,
    },
    /// 一轮解码失败。
    TurnFailed {
        /// 目标 run。
        run: Entity,
        /// 失败原因。
        error: String,
    },
}

/// 引擎运行时资源：持有工作线程与两侧通道。
#[derive(Resource)]
pub struct NeedleRuntime {
    job_tx: Sender<RuntimeJob>,
    // std Receiver 不是 Sync，用 Mutex 包装以满足 Resource: Sync
    event_rx: Mutex<Receiver<RuntimeEvent>>,
    _worker: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for NeedleRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NeedleRuntime").finish_non_exhaustive()
    }
}

impl NeedleRuntime {
    /// 启动工作线程并接管后端（Arc 共享：worker 独占使用）。
    pub fn new(backend: std::sync::Arc<dyn NeedleBackend>) -> Self {
        let (job_tx, job_rx) = mpsc::channel::<RuntimeJob>();
        let (event_tx, event_rx) = mpsc::channel::<RuntimeEvent>();

        let worker = std::thread::Builder::new()
            .name("bevy_needle_engine".into())
            .spawn(move || worker_loop(backend, job_rx, event_tx))
            .ok();

        Self {
            job_tx,
            event_rx: Mutex::new(event_rx),
            _worker: worker,
        }
    }

    /// 提交一轮解码（通道断开返回 false）。
    pub fn submit_turn(&self, job: TurnJob) -> bool {
        self.job_tx.send(RuntimeJob::Turn(job)).is_ok()
    }

    /// 请求引擎会话重置。
    pub fn submit_reset(&self) -> bool {
        self.job_tx.send(RuntimeJob::Reset).is_ok()
    }

    /// 收割本轮所有已完成事件（非阻塞）。
    pub fn drain_events(&self) -> Vec<RuntimeEvent> {
        let mut events = Vec::new();
        let Ok(rx) = self.event_rx.lock() else {
            return events;
        };
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }
}

fn worker_loop(
    backend: std::sync::Arc<dyn NeedleBackend>,
    job_rx: Receiver<RuntimeJob>,
    event_tx: Sender<RuntimeEvent>,
) {
    let mut last_signature: Option<u64> = None;
    let mut buffer = vec![0u8; backend.buffer_size()];

    while let Ok(job) = job_rx.recv() {
        match job {
            RuntimeJob::Reset => {
                backend.reset();
                last_signature = None; // 保险起见，下次强制重新绑定工具集
            }
            RuntimeJob::Turn(job) => {
                if buffer.len() < job.buffer_size {
                    buffer = vec![0u8; job.buffer_size];
                }
                if last_signature != Some(job.signature)
                    && let Err(err) = backend.bind(
                        job.signature,
                        &job.system,
                        &job.tools_json,
                        job.tool_index.as_deref(),
                    )
                {
                    let _ = event_tx.send(RuntimeEvent::TurnFailed {
                        run: job.run,
                        error: err.to_string(),
                    });
                    continue;
                }
                last_signature = Some(job.signature);

                match backend.complete(&job.input, job.max_new_tokens, &mut buffer) {
                    Ok(response) => {
                        let _ = event_tx.send(RuntimeEvent::TurnCompleted {
                            run: job.run,
                            response,
                        });
                    }
                    Err(err) => {
                        let _ = event_tx.send(RuntimeEvent::TurnFailed {
                            run: job.run,
                            error: err.to_string(),
                        });
                        // 绑定可能已被污染，强制下次重新初始化
                        last_signature = None;
                    }
                }
            }
        }
    }
}

/// 把 [`NeedleError`] 归一为可读字符串（工作线程事件用）。
pub fn describe_error(err: &NeedleError) -> String {
    err.to_string()
}
