//! DriverCoordinator：升级链路的调度核心（PR-B 新增，规格 §4/§6/§7）。
//!
//! **职责边界**（不变量 I9：`finalize_escalations` 是安全阀，不是业务逻辑）：
//! - ✅ 它做：drain 事件、把 `Escalating` run 提交给对应 tier 的 driver、
//!   按终态规则收尾（Completed / 下一档 / Failed / 取消）；
//! - ❌ 它不做：选「哪个 provider」、重试几次、是否用 rig——那些是
//!   [`DriverRegistry`]（tier→DriverId 映射）与 driver 自己的事。
//!
//! **三条接线约定**（规格 §3）：
//! 1. 本系统必须排在 `finalize_escalations` **之前**（否则永远等不到接管）；
//! 2. `tier` 是 `EscalationPolicy.tiers` 的**下标**，取 `tiers[tier]` 即可（I10）；
//! 3. `Escalating` 是中间态：输出侧只有 Completed / 下一档 Escalating / Failed。
//!
//! **终态归属**（规格 §6）：「没试/不许试」→ `Escalated`（安全阀收尾）；
//! 「试过且坏了」→ `Failed`；「用户喊停」→ `Cancelled`。

use std::time::Instant;

use bevy_ecs::prelude::*;
use std::sync::Arc;

use crate::agent::AgentToolRefs;
use crate::policy::EscalationPolicy;
use crate::run::{
    mark_run_completed, mark_run_failed, RunLastResponse, RunOwner, RunSession, RunStatus,
};
use crate::session::collect_transcript;
use crate::tool::ToolSpec;

use super::bus::DriverEventBus;
use super::driver::{
    Driver, DriverAttemptCtx, DriverError, DriverEvent, DriverId, DriverOutcome, EscalationReason,
};
use super::registry::DriverRegistry;
use super::state::EscalationState;

/// 升级调度系统（每帧；`world: &mut World` 独占）。
///
/// 顺序：drain 事件 → 提交新升级 → 超时检查 → 取消检查。
pub fn driver_coordinator(world: &mut World) {
    // 1. drain + process（stale epoch 在此丢弃，I17）
    let events = world.resource::<DriverEventBus>().drain();
    for event in events {
        process_event(world, event);
    }

    // 2. 提交新的升级（Escalating 但还没有 EscalationState）
    submit_fresh(world);

    // 3. 单档超时（per_tier_timeout）→ 视为该档失败，进下一档或终态
    check_timeouts(world);

    // 4. 协作式取消在途 attempt
    cancel_inflight(world);
}

/// 处理一条回灌事件（I17：epoch 不匹配直接丢弃，不改 Run）。
fn process_event(world: &mut World, event: DriverEvent) {
    // stale guard：run 必须还在 Escalating 且 epoch 匹配
    let Some(state) = world.get::<EscalationState>(event.run).cloned() else {
        return; // 从未属于我们 / 已被移除
    };
    let Some(status) = world.get::<RunStatus>(event.run).copied() else {
        return;
    };
    if !matches!(status, RunStatus::Escalating { .. }) {
        return; // 已终态（取消/完成）——旧响应不复活 Run（I17）
    }
    if state.epoch != event.epoch {
        return; // stale event：正常并发控制，不是错误（规格 §7）
    }

    match event.outcome {
        DriverOutcome::Succeeded { output } => {
            mark_run_completed(world, event.run, output);
        }
        DriverOutcome::Failed { error } => {
            fail_after_attempt(
                world,
                event.run,
                state.tier,
                state.epoch,
                state.reason,
                state.started,
                error,
            );
        }
    }
}

/// 提交尚未接管的升级（`Escalating` 且无 [`EscalationState`]）。
///
/// 无可用 driver / policy 禁止 / feature 关闭 → **不动**，留给
/// `finalize_escalations` 安全阀收尾为 `Escalated`（「没试/不许试」，规格 §6）。
fn submit_fresh(world: &mut World) {
    let policy = world.resource::<EscalationPolicy>().clone();
    if !policy.enabled {
        return;
    }

    let fresh: Vec<(Entity, u32)> = {
        let mut query = world.query::<(Entity, &RunStatus, Option<&EscalationState>)>();
        query
            .iter(world)
            .filter_map(|(run, status, state)| match status {
                RunStatus::Escalating { tier } if state.is_none() => Some((run, *tier)),
                _ => None,
            })
            .collect()
    };

    for (run, tier) in fresh {
        let Some(target) = policy.tiers.get(tier as usize).copied() else {
            continue; // 档位表外 → 安全阀收尾 Escalated
        };
        if !policy.allows(target) {
            continue; // policy 禁止 → 安全阀收尾 Escalated
        }
        if !world.resource::<DriverRegistry>().has_driver(tier) {
            // 规格 §6：「没试/不许试」→ Escalated——无可用 driver 时不许标记
            // Failed，留给 finalize_escalations 安全阀收尾（I9）。
            continue;
        }

        let now = Instant::now();
        match submit_tier(
            world,
            run,
            tier,
            0,
            0,
            EscalationReason::BelowConfidence,
            now,
        ) {
            Ok(()) => {}
            Err(error) => {
                // driver 已注册但 submit 自身报错 → 「试过且坏了」，进下一档或 Failed
                fail_after_attempt(world, run, tier, 0, EscalationReason::BelowConfidence, now, error);
            }
        }
    }
}

/// 提交某 tier 的第 `attempt` 次尝试。成功后把 [`EscalationState`]（含 handle）
/// 落到 run 实体；失败返回错误（由调用方决定进下一档或终态）。
fn submit_tier(
    world: &mut World,
    run: Entity,
    tier: u32,
    attempt: u32,
    epoch: u64,
    reason: EscalationReason,
    started: Instant,
) -> Result<(), DriverError> {
    let driver = world
        .resource::<DriverRegistry>()
        .resolve(tier)
        .ok_or_else(|| DriverError::Unavailable(format!("no driver registered for tier {tier}")))?;

    let policy = world.resource::<EscalationPolicy>().clone();
    let deadline = Instant::now() + policy.per_tier_timeout;

    let ctx = build_ctx(world, run, tier, attempt, epoch, deadline, reason);
    let bus = world.resource::<DriverEventBus>();
    let handle = driver.submit(ctx, &bus)?;

    let mut state = EscalationState::new(tier, driver.id(), epoch, reason);
    state.attempt = attempt;
    state.deadline = deadline;
    state.started = started;
    state.handle = Some(handle);

    if let Ok(mut entity) = world.get_entity_mut(run) {
        entity.insert((RunStatus::Escalating { tier }, state));
    }
    Ok(())
}

/// 某 tier 执行失败后的推进：有下一档 → `Escalating { tier+1 }`（attempt 归零、
/// epoch+1）；否则 `Failed`（「试过且坏了」，规格 §6）。
fn fail_after_attempt(
    world: &mut World,
    run: Entity,
    failed_tier: u32,
    epoch: u64,
    reason: EscalationReason,
    started: Instant,
    mut error: DriverError,
) {
    let mut tier = failed_tier;
    let mut epoch = epoch;

    loop {
        let policy = world.resource::<EscalationPolicy>().clone();

        // 总预算超时 → 直接终态，不再升档（规格 §7）
        if started.elapsed() >= policy.total_budget {
            mark_run_failed(
                world,
                run,
                format!("escalation total budget exceeded at tier {tier}: {error}"),
            );
            return;
        }

        let next = tier + 1;
        if (next as usize) >= policy.tiers.len() {
            // 档位耗尽且末档执行过 → Failed
            mark_run_failed(
                world,
                run,
                format!("driver failed at final tier {tier}: {error}"),
            );
            return;
        }

        let target = policy.tiers[next as usize];
        if !policy.allows(target) {
            mark_run_failed(
                world,
                run,
                format!("policy forbids tier {next} after failure at tier {tier}: {error}"),
            );
            return;
        }

        epoch = epoch.wrapping_add(1);

        // 提交下一档 attempt 0；提交失败视为该档「试过且坏了」，继续推进
        match submit_tier(world, run, next, 0, epoch, reason, started) {
            Ok(()) => return,
            Err(err) => {
                tier = next;
                error = err;
            }
        }
    }
}

/// 单档超时检查（`per_tier_timeout`）——超时视为该档失败，进下一档（规格 §7）。
fn check_timeouts(world: &mut World) {
    let now = Instant::now();
    let expired: Vec<(Entity, EscalationState)> = {
        let mut query = world.query::<(Entity, &RunStatus, &EscalationState)>();
        query
            .iter(world)
            .filter_map(|(run, status, state)| match status {
                RunStatus::Escalating { .. } if state.deadline <= now => Some((run, state.clone())),
                _ => None,
            })
            .collect()
    };

    for (run, state) in expired {
        let error = DriverError::Model(format!("tier {} timed out", state.tier));
        fail_after_attempt(world, run, state.tier, state.epoch, state.reason, state.started, error);
    }
}

/// 协作式取消在途 attempt（规格 §7：取消是逻辑取消，不保证底层中断）。
fn cancel_inflight(world: &mut World) {
    let cancels: Vec<(Entity, DriverId, super::driver::AttemptHandle)> = {
        let mut query = world.query::<(Entity, &RunStatus, &EscalationState)>();
        query
            .iter(world)
            .filter_map(|(run, status, state)| match status {
                RunStatus::Cancelled => state.handle.map(|h| (run, state.driver_id, h)),
                _ => None,
            })
            .collect()
    };

    // 先收集 driver 句柄（解除对 registry 的不可变借用），再取消 + 清 handle
    let drivers: Vec<Option<Arc<dyn Driver>>> = {
        let registry = world.resource::<DriverRegistry>();
        cancels
            .iter()
            .map(|(_, id, _)| registry.get(*id))
            .collect()
    };

    for ((run, _, handle), driver) in cancels.into_iter().zip(drivers) {
        if let Some(driver) = driver {
            driver.cancel(handle);
        }
        if let Ok(mut entity) = world.get_entity_mut(run) {
            if let Some(mut state) = entity.get_mut::<EscalationState>() {
                state.handle = None; // 幂等：不再重复取消
            }
        }
    }
}

/// 从 World 收集一次 attempt 的上下文快照（I14：转录按 `ChatMessageSeq` 排序）。
fn build_ctx(
    world: &mut World,
    run: Entity,
    tier: u32,
    attempt: u32,
    epoch: u64,
    deadline: Instant,
    reason: EscalationReason,
) -> DriverAttemptCtx {
    let session = world.get::<RunSession>(run).map(|s| s.0);
    let agent = world.get::<RunOwner>(run).map(|o| o.0);
    let transcript = session
        .map(|s| collect_transcript(world, s))
        .unwrap_or_default();
    let tools = agent.map(|a| collect_tools(world, a)).unwrap_or_default();
    let last_response = world
        .get::<RunLastResponse>(run)
        .and_then(|r| r.0.clone());

    DriverAttemptCtx {
        run,
        session: session.unwrap_or(Entity::PLACEHOLDER),
        agent: agent.unwrap_or(Entity::PLACEHOLDER),
        tier,
        attempt,
        epoch,
        deadline,
        reason,
        transcript,
        tools,
        last_response,
    }
}

/// 该 agent 的工具集快照（定义侧；执行永远在 ECS，I3）。
fn collect_tools(world: &mut World, agent: Entity) -> Vec<ToolSpec> {
    let Some(refs) = world.get::<AgentToolRefs>(agent) else {
        return Vec::new();
    };
    refs.0
        .iter()
        .filter_map(|entity| world.get::<ToolSpec>(*entity).cloned())
        .collect()
}
