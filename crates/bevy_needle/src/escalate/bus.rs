//! 升级事件总线：driver → ECS 的 channel 回灌（I16）。
//!
//! 与 `needle_runtime` 同款形态：`Sender` 克隆安全（driver worker 侧持有），
//! `Receiver` 包 `Mutex` 以满足 `Resource: Sync`；ECS 每帧 `drain` 非阻塞收割。

use std::sync::mpsc::{Receiver, Sender, TryRecvError};
use std::sync::Mutex;

use bevy_ecs::prelude::Resource;

use super::driver::DriverEvent;

/// 升级事件的回灌总线（Resource）。
#[derive(Resource)]
pub struct DriverEventBus {
    tx: Sender<DriverEvent>,
    rx: Mutex<Receiver<DriverEvent>>,
}

impl DriverEventBus {
    /// 构造/执行入口（错误经 `Result` 返回，不 panic）。
    pub fn new() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            tx,
            rx: Mutex::new(rx),
        }
    }

    /// 发送端（driver worker 侧 clone 持有）。
    pub fn sender(&self) -> Sender<DriverEvent> {
        self.tx.clone()
    }

    /// 非阻塞收割本轮所有事件（ECS 每帧一次；无 poll，I16）。
    pub fn drain(&self) -> Vec<DriverEvent> {
        let mut events = Vec::new();
        let Ok(rx) = self.rx.lock() else {
            return events;
        };
        loop {
            match rx.try_recv() {
                Ok(event) => events.push(event),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        events
    }
}

impl Default for DriverEventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DriverEventBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DriverEventBus")
    }
}
