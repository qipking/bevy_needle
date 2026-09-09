//! RigDriver：把 rig-run 的 `AgentRun` 适配成 [`Driver`]（规格 §5）。
//!
//! 关键点：rig 的 `AgentRun` 是 **ECS 组件**（[`super::agent::RigDriverState`]），
//! 步进由 [`super::agent::rig_step_system`] 完成（I4：Bevy Run ⊃ AgentRun；
//! I3：工具执行在 ECS）。因此本 `Driver` 的 `submit` 只回执句柄——真正的
//! 接管发生在 rig 系统看到 `EscalationState.driver_id == rig` 且无
//! `RigDriverState` 时（下一帧）。
//!
//! # 模型所有权（P1-7 裁决 + I24）
//!
//! [`rig_core::completion::CompletionModel`] 是 **RPITIT**（`fn completion(
//! &self, ..) -> impl Future`）——**非 dyn-compatible**，`Box<dyn CompletionModel>`
//! 不存在。因此模型以泛型参数 `M: CompletionModel` 进入 [`RigDriver`]，
//! `Arc<M>` 共享（trait 自带 `Arc<M>` forwarding impl，"wrap it in an Arc"
//! 是上游文档原话）。
//!
//! I24：`M` **不渗透** `Driver` trait——`Driver` 只认 `DriverId`/`run`/`epoch`；
//! 模型是什么是 [`RigDriver`] 的实现细节，经 `ModelJob` 私有通道交给
//! worker / 步进系统（`completion().await` 是 worker 侧唯一 async 点，I22）。
//!
//! # 「Lazy」的合规形态（规格 §13 预热纪律）
//!
//! **启动期预热，而非用时才建**：`Arc<M>` 在构造 [`RigDriver`] 时即已就绪
//! （宿主在 startup / `Plugin::finish()` 完成秒级不可取消的模型加载），
//! `submit` 只 clone `Arc`——首次升级零加载。预热失败 → 该 tier 不注册，
//! 按 §6 走 `Escalated`（没试），不是 `Failed`（试过且坏）。

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use rig_core::completion::CompletionModel;

use crate::escalate::driver::{
    AttemptHandle, Driver, DriverAttemptCtx, DriverError, DriverId,
};

use super::agent::{ModelJob, RigModelInbox};

static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

/// rig 适配 Driver（泛型模型；见模块级文档 P1-7/I24）。
///
/// **I30：身份是实例字段**——`id` / `capability` 都在构造期传入，不存在
/// 固定常量（v14 §16.1 的三处违规之一就在这里修掉）。多个 `RigDriver<M>`
/// 实例（不同 id，甚至同一 `Arc<M>`）可以共存，一个实例可绑多个 tier（I32）。
pub struct RigDriver<M: CompletionModel + 'static> {
    /// 本实例的身份（I29：身份归 Driver 实例所有）。
    id: DriverId,
    /// 能力类别（I25：与绑定 tier 的 `EscalationTarget` 在解析期校验）。
    capability: crate::policy::EscalationTarget,
    /// 启动期预热的共享模型（`submit` 时随 job 交给 worker 侧执行）。
    model: Arc<M>,
    /// 模型作业通道（与 [`RigModelInbox`] 同一条回灌链路的请求侧）。
    jobs: Sender<ModelJob<M>>,
    /// 本实例被 cancel 的次数（每实例独立——I28 取消路由的可观测依据）。
    cancelled: Arc<std::sync::atomic::AtomicUsize>,
}

impl<M: CompletionModel + 'static> RigDriver<M> {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    ///
    /// - `id`：本实例的身份（宿主起名，如 `"rig-candle"` / `"rig-openai"`；
    ///   重复 id 会在 `DriverRegistry::register` 处报 `DuplicateDriverId`，I29）；
    /// - `capability`：能力类别（I25——`Remote` 模型必须声明 `Remote`，
    ///   硬编码 `Local` 会让远端档永远注册不上，§16.3）；
    /// - `model` 必须已在启动期构造完成（§13：不在升级路径上做秒级加载）；
    /// - `inbox` 是模型回合的**唯一注入点**（I26）；其请求侧通道在这里接走
    ///   （同一 `M` 的多个实例共享同一 inbox，I32）。
    pub fn new(
        id: DriverId,
        capability: crate::policy::EscalationTarget,
        model: Arc<M>,
        inbox: &RigModelInbox<M>,
    ) -> Self {
        Self {
            id,
            capability,
            model,
            jobs: inbox.jobs(),
            cancelled: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// 本实例被 cancel 的次数（I28 取消路由测试的可观测依据）。
    pub fn cancelled_count(&self) -> usize {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl<M: CompletionModel + 'static> std::fmt::Debug for RigDriver<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RigDriver").finish_non_exhaustive()
    }
}

impl<M: CompletionModel + 'static> Driver for RigDriver<M> {
    /// I30：返回**实例字段**，不是常量。
    fn id(&self) -> DriverId {
        self.id
    }

    /// I30：返回**实例字段**（I25 校验依据；硬编码会让 Remote 档注册不上）。
    fn capability(&self) -> crate::policy::EscalationTarget {
        self.capability
    }

    fn submit(
        &self,
        ctx: DriverAttemptCtx,
        _bus: &crate::escalate::DriverEventBus,
    ) -> Result<AttemptHandle, DriverError> {
        // I17：job 携带 (run, epoch)；通道断开 → TransportError（§7）。
        self.jobs
            .send(ModelJob::Attach {
                run: ctx.run,
                epoch: ctx.epoch,
                model: Arc::clone(&self.model),
            })
            .map_err(|_| DriverError::Transport("rig model worker channel closed".into()))?;
        // 实际接管由 rig_step_system 完成（AgentRun 是 ECS 组件，I4/I3）。
        Ok(AttemptHandle(NEXT_HANDLE.fetch_add(1, Ordering::Relaxed)))
    }

    fn cancel(&self, _handle: AttemptHandle) {
        self.cancelled
            .fetch_add(1, Ordering::SeqCst);
        // 协作式取消：rig 系统按 RunStatus 与 epoch 失效丢弃在途结果（I17）。
        // 底层模型中断不保证（candle load 不可中止，规格 §7）。
    }
}
