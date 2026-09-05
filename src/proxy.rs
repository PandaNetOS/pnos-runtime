//! 反向代理
//!
//! 将 /app/{id}/* 请求转发到对应应用的端口。

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, Method, Request, StatusCode, Uri};
use axum::response::Response;
use bytes::Bytes;
use futures_util::StreamExt;

use crate::config::AppState;
use std::sync::Arc;

/// 反向代理处理函数
pub async fn proxy_handler(
    State(state): State<Arc<AppState>>,
    Path(app_id): Path<String>,
    method: Method,
    headers: HeaderMap,
    uri: Uri,
    body: Bytes,
) -> Result<Response<Body>, StatusCode> {
    // 从注册中心查找应用
    let app = state
        .registry
        .get(&app_id)
        .await
        .ok_or(StatusCode::NOT_FOUND)?;

    // 构建目标 URL：剥离 /app/{id} 前缀
    let path = uri.path();
    let prefix = format!("/app/{}", app_id);
    let remaining = path.strip_prefix(&prefix).unwrap_or("/");
    let query = uri.query().map(|q| format!("?{}", q)).unwrap_or_default();
    let target_url = format!("http://127.0.0.1:{}{}{}", app.port, remaining, query);

    // 转发请求
    let client = &state.http_client;
    let mut req = client.request(method, &target_url).body(body);

    // 转发请求头（跳过 host）
    for (key, value) in headers.iter() {
        if key != axum::http::header::HOST {
            req = req.header(key, value);
        }
    }

    let resp = req
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;

    // 构建响应
    let mut builder = Response::builder().status(resp.status());
    for (key, value) in resp.headers() {
        builder = builder.header(key, value);
    }

    let body_stream = resp.bytes_stream().map(|result| {
        result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    });
    let body = Body::from_stream(body_stream);

    builder
        .body(body)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}
