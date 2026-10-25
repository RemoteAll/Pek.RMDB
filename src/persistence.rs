//! 持久化调度原语：**变化驱动 + 写合并**。
//!
//! 设计目标（对齐用户“资源与带宽”偏好）：
//! - **变化驱动**：只有标记为脏的表才会被写盘，空闲时后台线程在条件变量上无限等待（零轮询）；
//! - **写合并**：500ms 最小间隔内的多次修改合并为一次序列化 + 一次写盘；
//! - **原子替换**：原子写复用 `dhrust::io::write_all_text_atomic`（临时文件 + 替换）；
//! - **不丢尾写**：写盘开始时“取走”脏标记，写入期间的新修改会重新置脏并在下一轮补写。

use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// 后台写盘调度原语（条件变量 + 最小写入间隔）。
pub(crate) struct PersistHub {
    mutex: Mutex<HubState>,
    cv: Condvar,
}

struct HubState {
    pending: bool,
    stop: bool,
    last_write: Option<Instant>,
}

impl PersistHub {
    pub(crate) fn new() -> Self {
        Self {
            mutex: Mutex::new(HubState {
                pending: false,
                stop: false,
                last_write: None,
            }),
            cv: Condvar::new(),
        }
    }

    /// 有新脏表（幂等；已有 pending 时不重复唤醒）。
    pub(crate) fn notify(&self) {
        let mut state = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        if !state.pending && !state.stop {
            state.pending = true;
            self.cv.notify_all();
        }
    }

    /// 停止后台线程并唤醒等待。
    pub(crate) fn stop(&self) {
        let mut state = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        state.stop = true;
        state.pending = true;
        self.cv.notify_all();
    }

    /// 等待“有脏表/停止”信号（无脏表时无限等待，零轮询）。
    pub(crate) fn wait(&self) {
        let mut state = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        while !state.pending && !state.stop {
            state = self.cv.wait(state).unwrap_or_else(|e| e.into_inner());
        }
    }

    pub(crate) fn take_pending(&self) {
        let mut state = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        state.pending = false;
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.mutex
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stop
    }

    /// 距上次写盘不足最小间隔时补等剩余时间（后台线程的 sleep，不忙等）。
    pub(crate) fn wait_min_interval(&self, interval: Duration) {
        if interval.is_zero() {
            return;
        }
        let wait = {
            let state = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
            state
                .last_write
                .map(|last| interval.saturating_sub(last.elapsed()))
                .unwrap_or_default()
        };
        if !wait.is_zero() {
            std::thread::sleep(wait);
        }
    }

    pub(crate) fn mark_written(&self) {
        let mut state = self.mutex.lock().unwrap_or_else(|e| e.into_inner());
        state.last_write = Some(Instant::now());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hub_notify_and_stop_are_idempotent() {
        let hub = PersistHub::new();
        hub.notify();
        hub.notify();
        hub.take_pending();
        hub.mark_written();
        hub.stop();
        hub.stop();
        assert!(hub.is_stopped());
    }
}
