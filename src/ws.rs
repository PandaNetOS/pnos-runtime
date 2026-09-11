//! WebSocket 事件总线与端点
//!
//! 提供全局事件总线（`broadcast`），供 runtime 各模块发布生命周期事件；
//! 暴露 `/api/v1/ws` 端点，pnos-comm 的 `WsClient` 可连接并按 `WsSubscribe` 订阅事件。
//! 协议与 pnos-spec `events` 模块对齐：订阅用 `WsSubscribe`，推送用 `WsMessage`。

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use pnos::events::{WsMessage, WsSubscribe};
use tokio::sync::broadcast;
use tokio::sync::mpsc;
use tokio::sync::Mutex as TokioMutex;

use crate::config::AppState;

/// 广播通道容量（慢消费者会丢弃旧事件，事件允许丢失）
const BUS_CAPACITY: usize = 1024;

struct EventBus {
    tx: broadcast::Sender<WsMessage>,
}

static EVENT_BUS: OnceLock<EventBus> = OnceLock::new();

/// 初始化全局事件总线（须在 AppState::new 中调用一次）
pub fn init() {
    let (tx, _rx) = broadcast::channel::<WsMessage>(BUS_CAPACITY);
    let _ = EVENT_BUS.set(EventBus { tx });
}

/// 发布事件（来源默认为 "system"）
pub fn publish(event_type: &str, payload: serde_json::Value) {
    if let Some(bus) = EVENT_BUS.get() {
        let _ = bus.tx.send(WsMessage::new(event_type, payload));
    }
}

/// 发布事件并指定来源组件 ID
pub fn publish_with_source(event_type: &str, source: &str, payload: serde_json::Value) {
    if let Some(bus) = EVENT_BUS.get() {
        let _ = bus
            .tx
            .send(WsMessage::new(event_type, payload).with_source(source));
    }
}

/// WebSocket 握手入口：校验 token，失败立即拒绝
pub async fn ws_handler(
    ws: WebSocketUpgrade,
    Query(params): Query<std::collections::HashMap<String, String>>,
    State(state): State<Arc<AppState>>,
) -> Response {
    let token = params.get("token").cloned().unwrap_or_default();
    if !state.registry.token_valid(&token).await {
        return (
            axum::http::StatusCode::FORBIDDEN,
            "invalid or missing token",
        )
            .into_response();
    }
    ws.on_upgrade(move |socket| handle_socket(socket))
}

/// 单连接处理：拆分 sink/source，订阅管理 + 事件转发 + Ping-Pong
async fn handle_socket(socket: WebSocket) {
    let (mut sender, mut receiver) = socket.split();
    let subscriptions: Arc<TokioMutex<HashSet<String>>> = Arc::new(TokioMutex::new(HashSet::new()));

    let bus = match EVENT_BUS.get() {
        Some(b) => b,
        None => {
            let _ = sender.close().await;
            return;
        }
    };
    let mut rx = bus.tx.subscribe();

    // 命令通道：主任务把需要发送的消息（Pong）转给转发任务
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<Message>(32);

    // 转发任务：消费广播事件，按订阅过滤后写入 sink；同时处理 Pong 命令
    let subs_fwd = subscriptions.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                evt = rx.recv() => {
                    match evt {
                        Ok(msg) => {
                            let matched = {
                                let subs = subs_fwd.lock().await;
                                event_matches(&msg.event_type, &subs)
                            };
                            if matched {
                                if let Ok(json) = serde_json::to_string(&msg) {
                                    if sender.send(Message::Text(json)).await.is_err() {
                                        break;
                                    }
                                }
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!("ws client lagged, dropped {} events", n);
                        }
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(m) => {
                            if sender.send(m).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
            }
        }
    });

    // 主任务：读取客户端消息（订阅 / 退订 / Ping / Close）
    while let Some(Ok(msg)) = receiver.next().await {
        match msg {
            Message::Text(text) => {
                if let Ok(sub) = serde_json::from_str::<WsSubscribe>(&text) {
                    let mut subs = subscriptions.lock().await;
                    match sub.action.as_str() {
                        "subscribe" => {
                            subs.insert(sub.event_type);
                        }
                        "unsubscribe" => {
                            subs.remove(&sub.event_type);
                        }
                        _ => {}
                    }
                }
            }
            Message::Ping(payload) => {
                let _ = cmd_tx.send(Message::Pong(payload)).await;
            }
            Message::Pong(_) => {}
            Message::Close(_) => break,
            _ => {}
        }
    }
}

/// 事件类型与订阅模式匹配（对齐 pnos-comm events.rs 逻辑）
fn event_matches(event_type: &str, subs: &HashSet<String>) -> bool {
    for pat in subs {
        if pat == "*" {
            return true;
        }
        if let Some(prefix) = pat.strip_suffix('*') {
            if event_type.starts_with(prefix) {
                return true;
            }
        } else if pat == event_type {
            return true;
        }
    }
    false
}
