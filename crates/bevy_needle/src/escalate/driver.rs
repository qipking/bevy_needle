//! Driver 契约（PR-B 核心新增，规格 §4）。
//!
//! **分层职责（不变量 I5/I7/I6）**：
//! ```text
//! EscalationPolicy  决定「是否升级、升到哪个 tier」  ← 纯决策（core，无 cfg）
//! DriverRegistry    负责「tier → 哪个 Driver」        ← 持有执行者（本层）
//! Driver            负责「怎么执行」                   ← 纯执行（本 trait）
//! Model             真正产生推理结果                   ← Driver 内部使用
//! ```
//!
//! **为什么没有 `poll`（I16）**：`poll(&mut AttemptHandle) -> Option<DriverEvent>`
//! 会强迫 Driver 自己保存 Future、管理 waker、感知 runtime——等于在 Driver 里
//! 重造一个简化版 executor，且异步细节泄漏给 ECS 主循环。正确形态：
//! `submit` → worker spawn → channel → ECS `drain_events`。**ECS 只消费，不推进。**
//!
//! 注意：`poll` 并非一律禁止——rig #2446 的 `Pending::poll_outcome` 是对
//! **sans-I/O 状态机**的 poll，合法且归 transport 适配层；禁止的是**对 async
//! future 的 poll 出现在 Driver 契约面**（那归 worker，I22）。

use std::time::Instant;

use bevy_ecs::prelude::Entity;
use serde_json::Value;

use crate::session::ChatMessageRole;
use crate::tool::ToolSpec;

use super::bus::DriverEventBus;

/// Driver 标识（I6：`RunResolutionSystems` 只认它，不认 Rig/OpenAI/Candle）。
///
/// 用 id 而非 enum，是为了让第三方 crate 可注册新 driver 而不改核心。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DriverId(pub &'static str);

impl DriverId {
    /// `&'static str` 形态。
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for DriverId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// 一次 attempt 的句柄（driver 铸造；opaque 给 ECS，用于协作式取消）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct AttemptHandle(pub u64);

/// 升级原因。v1 只支持「推理升级」（规格 §6 语义澄清）：
/// 模型回答 confidence < threshold → 不执行工具 → 换更强 driver 重新推理。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscalationReason {
    /// 置信度低于门限（`RunEscalation` 触发）。
    BelowConfidence,
}

/// 一次 attempt 的上下文（coordinator 从 World 收集后交给 driver）。
///
/// 注意：这里携带的是**数据快照**（转录、工具集），不携带 World / Entity 之外的
/// 生命周期句柄；driver 据此可完全脱离 ECS 在 worker 侧推进。
#[derive(Clone, Debug)]
pub struct DriverAttemptCtx {
    /// 目标 run。
    pub run: Entity,
    /// 目标会话（转录归属）。
    pub session: Entity,
    /// run 的 owner agent。
    pub agent: Entity,
    /// `EscalationPolicy.tiers` 下标（I10：下标，不是 rank）。
    pub tier: u32,
    /// 当前 tier 内的第几次尝试（0 起；重试不增 tier）。
    pub attempt: u32,
    /// 防 stale event（I17）。
    pub epoch: u64,
    /// `per_tier_timeout` 的到期时刻。
    pub deadline: Instant,
    /// 为什么升级。
    pub reason: EscalationReason,
    /// 首次交接快照（按 `ChatMessageSeq` 排序，I14）。
    pub transcript: Vec<(ChatMessageRole, String)>,
    /// 该 agent 的工具集快照（定义侧；执行永远在 ECS，I3）。
    pub tools: Vec<ToolSpec>,
    /// needle 最近一次信封（上下文延续用；driver 可选消费）。
    pub last_response: Option<Value>,
}

/// 错误分类（规格 §7，禁止全部压成 `Engine(..)`）。
#[derive(Clone, Debug)]
pub enum DriverError {
    /// 无匹配 driver / driver 未注册（`DriverUnavailable`）。
    Unavailable(String),
    /// 协议错误（call_id 缺失、重复 ID、非法状态转移）。
    Protocol(DriverProtocolError),
    /// channel 断开、worker panic。
    Transport(String),
    /// 模型返回错误。
    Model(String),
    /// 逻辑取消。
    Cancelled,
}

impl std::fmt::Display for DriverError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriverError::Unavailable(msg) => write!(f, "driver unavailable: {msg}"),
            DriverError::Protocol(err) => write!(f, "driver protocol error: {err}"),
            DriverError::Transport(msg) => write!(f, "driver transport error: {msg}"),
            DriverError::Model(msg) => write!(f, "driver model error: {msg}"),
            DriverError::Cancelled => f.write_str("driver attempt cancelled"),
        }
    }
}

impl std::error::Error for DriverError {}

impl From<DriverProtocolError> for DriverError {
    fn from(err: DriverProtocolError) -> Self {
        DriverError::Protocol(err)
    }
}

/// 协议错误（规格 §7 错误分类）。
#[derive(Clone, Debug)]
pub enum DriverProtocolError {
    /// 工具 call_id 缺失（I18：禁止 mint 新 ID）。
    MissingToolCallId,
    /// 工具 call_id 重复（§8 测试矩阵：拒绝）。
    DuplicateToolCallId(String),
    /// 状态机非法转移。
    InvalidState(String),
}

impl std::fmt::Display for DriverProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DriverProtocolError::MissingToolCallId => f.write_str("missing tool call id"),
            DriverProtocolError::DuplicateToolCallId(id) => {
                write!(f, "duplicate tool call id: {id}")
            }
            DriverProtocolError::InvalidState(msg) => write!(f, "invalid state: {msg}"),
        }
    }
}

impl std::error::Error for DriverProtocolError {}

/// driver → ECS 的终态结果（channel 回灌，I16）。
///
/// 注意：`Escalating` 是中间态，出现在 `RunResolution` 的**输入侧**；输出侧只有
/// `Succeeded` / `Failed`（规格 §3）。
#[derive(Clone, Debug)]
pub enum DriverOutcome {
    /// 本 tier 成功：`output` 落入转录。
    Succeeded {
        /// 最终回复文本（转录落 `RunResultText`）。
        output: String,
    },
    /// 本 tier 失败（协议/传输/模型错误）。coordinator 决定进下一档还是终态。
    Failed {
        /// 失败原因。
        error: DriverError,
    },
}

/// 回灌事件（I17：必须携带 `(run, epoch)`；epoch 不匹配则丢弃，不得改 Run）。
#[derive(Clone, Debug)]
pub struct DriverEvent {
    /// 目标 run。
    pub run: Entity,
    /// 发起 attempt 时的 epoch。
    pub epoch: u64,
    /// 产生事件者。
    pub driver: DriverId,
    /// 终态结果。
    pub outcome: DriverOutcome,
}

/// Driver 契约（I2/I16）。
///
/// - 与工具系统完全正交（Driver ≠ ToolHandler ≠ NeedleBackend ≠ Agent）；
/// - 无 `poll`、无 Future、无 waker、无 runtime（I16/I22）——异步结果经
///   [`DriverEventBus`] 回灌，ECS 只 drain；
/// - `submit` 立即返回（I1：主线程永不 block_on）；worker 侧才允许 block_on。
pub trait Driver: Send + Sync + 'static {
    /// 本 driver 的标识（I6：RunResolutionSystems 只认它）。
    fn id(&self) -> DriverId;

    /// 提交一次 attempt。立即返回，不阻塞。
    ///
    /// `out` 是回灌通道的发送端（coordinator 持有接收端并 drain）；driver
    /// 可 clone 后在 worker 侧使用。结果**不经过返回值、不经过 poll**。
    ///
    /// # Errors
    /// 提交失败（driver 不可用 / 传输错误 / 协议错误）。
    fn submit(
        &self,
        ctx: DriverAttemptCtx,
        out: &DriverEventBus,
    ) -> Result<AttemptHandle, DriverError>;

    /// 协作式取消。不保证底层 model 中断（candle load 不可中止，规格 §7）。
    fn cancel(&self, handle: AttemptHandle);
}
