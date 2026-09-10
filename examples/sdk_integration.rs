//! pnos-comm 通信 SDK 接入 pnos-runtime 端到端验证
//!
//! 前置条件：pnos-runtime 已在 :8080 运行（提供 REST + /api/v1/ws）
//! 运行：
//!   cargo run --example sdk_integration
//!   或指定地址：PNOS_RUNTIME_URL=http://host:port cargo run --example sdk_integration
//!
//! 验证闭环：PnosApp.init() 自动 注册→心跳→WS 连接→事件分发；
//! 用一个「监听者」订阅 component.registered，再用一个「触发者」注册，
//! runtime 广播该事件，监听者经 WS 收到即证明 SDK→runtime 实时链路打通。

use std::sync::Arc;
use std::time::Duration;

use pnos::component::{ComponentStatus, ComponentType};
use pnos_comm::PnosApp;
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let runtime_url = std::env::var("PNOS_RUNTIME_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".to_string());

    println!("==> 目标 runtime: {}", runtime_url);

    // 监听者：订阅 component.registered，等待触发者注册时由 runtime 广播
    let received = Arc::new(Notify::new());
    let received_cb = received.clone();
    let listener = PnosApp::builder("sdk-listener")
        .version("0.1.0")
        .component_type(ComponentType::App)
        .runtime_url(&runtime_url)
        .no_auto_heartbeat()
        .on_event("component.registered", move |_evt| {
            let n = received_cb.clone();
            async move {
                tracing::info!("listener 收到 component.registered 事件");
                n.notify_one();
            }
        })
        .init()
        .await?;

    let listener_token = listener.token().await;
    anyhow::ensure!(
        listener_token.is_some(),
        "listener 注册失败：未获得 token（请确认 runtime 在 {} 运行）",
        runtime_url
    );
    let token_preview = listener_token
        .as_ref()
        .map(|t| &t[..t.len().min(8)])
        .unwrap_or("");
    println!("[1] 注册成功 (listener) token={}...", token_preview);

    listener.heartbeat(ComponentStatus::Running).await?;
    println!("[2] 心跳成功 (listener)");

    // 触发者：注册时会由 runtime 广播 component.registered
    let trigger = PnosApp::builder("sdk-trigger")
        .version("0.1.0")
        .component_type(ComponentType::App)
        .runtime_url(&runtime_url)
        .no_auto_heartbeat()
        .init()
        .await?;
    println!("[3] 注册成功 (trigger)，runtime 应已广播 component.registered");

    // 等待监听者通过 WS 收到事件
    let ws_ok = tokio::time::timeout(Duration::from_secs(5), received.notified())
        .await
        .is_ok();
    println!(
        "[4] WS 事件接收: {}",
        if ws_ok { "OK" } else { "TIMEOUT(未收到)" }
    );

    // 清理：触发关闭回调注销组件
    trigger.shutdown().await;
    listener.shutdown().await;

    println!(
        "\n==> 接入验证结果: register=OK  heartbeat=OK  ws_event={}",
        if ws_ok { "OK" } else { "FAIL" }
    );
    if !ws_ok {
        anyhow::bail!("WS 事件未收到，通信 SDK 接入验证失败");
    }
    Ok(())
}
