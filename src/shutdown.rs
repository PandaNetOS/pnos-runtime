//! 进程关闭广播
//!
//! 后台循环（心跳巡检 / 指标采集 / Agent 巡检）统一通过 [`subscribe`] 挂到同一份
//! 关闭信号上，收到信号后在下一次循环边界退出，避免进程被强杀时留下半写状态。

use std::sync::OnceLock;

use tokio::sync::watch;

static SHUTDOWN: OnceLock<watch::Sender<bool>> = OnceLock::new();

/// 初始化关闭广播（`main` 启动时调用一次）
pub fn init() {
    let (tx, _rx) = watch::channel(false);
    let _ = SHUTDOWN.set(tx);
}

/// 订阅关闭信号；未初始化时返回一个永不触发的接收端
pub fn subscribe() -> watch::Receiver<bool> {
    match SHUTDOWN.get() {
        Some(tx) => tx.subscribe(),
        None => watch::channel(false).1,
    }
}

/// 广播关闭（收到 Ctrl+C 等终止信号时调用）
pub fn trigger() {
    if let Some(tx) = SHUTDOWN.get() {
        let _ = tx.send(true);
    }
}
