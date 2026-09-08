//! 驱动传输层选择（R2 / R3 切换面，规格 §3.5）。
//!
//! 当前实现 = **R2**（模型调用经独立 worker + 懒创建 tokio，见 [`super::runtime`]）；
//! 当上游 `rig-bus` 的 `Pending::poll_outcome` / `EffectStream::poll_item` 上
//! main 并发布（#2446 目前在 `feat/effect-bus` 分支），模型调用可退化为 **R3**
//! （no-op waker 直 poll sans-I/O 状态机，无执行器）——切换点只在本文件，
//! 协议层（[`super::agent`]）与工具桥（[`super::tool_bridge`]）不动。
//! 这正是规格 §2.5「押协议层，不押驱动层」的落地。
//!
//! **PR-B 范围说明**：`CallModel` 的真实 provider 执行（异步模型调用）留待
//! 下一阶段——本层已提供 [`super::agent::RigModelInbox`] 回灌 seam；生产 worker
//! 在此文件落地时引入（`RigRuntime::block_on` 只在 worker 线程调用，I22）。
//!
//! R3 前置条件核对清单（届时逐条验证，未全绿不得切换）：
//! 1. `rig-bus` 发布到 crates.io（或 pin rev 稳定）；
//! 2. `Pending::poll_outcome` 确切签名（Q11——docs.rs 摘要会吞返回类型，须读源码）；
//! 3. `Dispatcher` / `Driver` 生命周期边界（Q12：谁持有谁、能否跨帧）。

/// 当前生效的传输层（编译期常量；供诊断展示）。
pub const CURRENT: &str = "R2-worker-thread";

/// R3 未落地前恒为 false（防误切）。
pub const BUS_POLL_AVAILABLE: bool = false;
