//! MockDriver：脚本化 driver（无 rig、无网络，跑通完整升级链路——§0 成功判据）。
//!
//! 语义对齐 `MockBackend`：按脚本顺序回放终态结果（耗尽后默认成功）。
//! 用于：
//! - `escalate` feature 单独启用时的状态机测试（无需 rig）；
//! - 游戏侧做「升级路径」教学与验收。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::bus::DriverEventBus;
use super::driver::{
    AttemptHandle, Driver, DriverAttemptCtx, DriverError, DriverEvent, DriverId, DriverOutcome,
};

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// 脚本条目：一次 attempt 的预设终态。
#[derive(Clone, Debug)]
pub enum MockStep {
    /// 成功，输出指定文本。
    Succeed {
        /// 落入转录的回复文本。
        output: String,
    },
    /// 失败（进下一档或终态）。
    Fail {
        /// 失败原因。
        error: DriverError,
    },
}

impl MockStep {
    /// 成功条目。
    pub fn succeed(output: impl Into<String>) -> Self {
        MockStep::Succeed {
            output: output.into(),
        }
    }

    /// 失败条目。
    pub fn fail(error: DriverError) -> Self {
        MockStep::Fail { error }
    }
}

/// 脚本化 driver：`submit` 立即把脚本里的下一条结果经 bus 回灌。
pub struct MockDriver {
    id: DriverId,
    script: Mutex<Vec<MockStep>>,
    cursor: Mutex<usize>,
    default: MockStep,
    cancelled: AtomicU64,
}

impl MockDriver {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new(id: &'static str) -> Self {
        Self {
            id: DriverId(id),
            script: Mutex::new(Vec::new()),
            cursor: Mutex::new(0),
            default: MockStep::Succeed {
                output: "mock done".into(),
            },
            cancelled: AtomicU64::new(0),
        }
    }

    /// 设置脚本（按序回放，耗尽后回放 `default`）。
    pub fn with_script(mut self, steps: Vec<MockStep>) -> Self {
        self.script = Mutex::new(steps);
        self
    }

    /// 设置脚本耗尽后的默认终态。
    pub fn with_default(mut self, step: MockStep) -> Self {
        self.default = step;
        self
    }

    /// 累计取消次数（测试断言协作式取消语义）。
    pub fn cancelled(&self) -> u64 {
        self.cancelled.load(Ordering::Relaxed)
    }
}

impl Driver for MockDriver {
    fn id(&self) -> DriverId {
        self.id
    }

    fn submit(
        &self,
        ctx: DriverAttemptCtx,
        bus: &DriverEventBus,
    ) -> Result<AttemptHandle, DriverError> {
        let handle = AttemptHandle(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed));
        let step = {
            let mut cursor = self.cursor.lock().expect("mock cursor poisoned");
            let script = self.script.lock().expect("mock script poisoned");
            let step = script
                .get(*cursor)
                .cloned()
                .unwrap_or_else(|| self.default.clone());
            *cursor += 1;
            step
        };
        let outcome = match step {
            MockStep::Succeed { output } => DriverOutcome::Succeeded { output },
            MockStep::Fail { error } => DriverOutcome::Failed { error },
        };
        // 同步回灌（立即返回，不阻塞；channel 无界，send 不会失败除非接收端 drop）
        let _ = bus.sender().send(DriverEvent {
            run: ctx.run,
            epoch: ctx.epoch,
            driver: self.id,
            outcome,
        });
        Ok(handle)
    }

    fn cancel(&self, _handle: AttemptHandle) {
        self.cancelled.fetch_add(1, Ordering::Relaxed);
    }
}

impl std::fmt::Debug for MockDriver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockDriver").field("id", &self.id).finish()
    }
}
