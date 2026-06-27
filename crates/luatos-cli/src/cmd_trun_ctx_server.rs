//! `trun` 命令的 ctx.json 回传监听器
//!
//! 启动一个轻量 HTTP 服务器，接收设备端 POST：
//! - `POST /status` — 设备进度回报 `{test_id, stage, percent, message}`
//! - `POST /result` — 设备最终结果 `{test_id, ok, message}`
//!
//! 任何非 POST / 错误路径 → 404
//! 跨 test_id 的 POST → 400

use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::oneshot;

/// 监听器接收到的事件
#[derive(Debug, Clone, Serialize)]
pub enum CtxEvent {
    /// 进度回报
    Status {
        test_id: String,
        stage: Option<String>,
        percent: Option<f32>,
        message: Option<String>,
        raw: Value,
    },
    /// 最终结果
    Result { test_id: String, ok: bool, message: Option<String>, raw: Value },
    /// 未知路径 / 格式
    #[allow(dead_code)]
    Unknown { path: String, raw: Value },
}

/// 监听器共享状态
#[derive(Clone)]
struct ServerState {
    test_id: String,
    events: Arc<Mutex<Vec<CtxEvent>>>,
}

/// 监听器句柄
pub struct CtxServerHandle {
    /// 实际监听端口（0 表示未启动）
    pub port: u16,
    /// 事件收集器
    pub events: Arc<Mutex<Vec<CtxEvent>>>,
    /// 关闭信号 sender
    shutdown: Option<oneshot::Sender<()>>,
    /// server task handle
    #[allow(dead_code)]
    join: Option<tokio::task::JoinHandle<()>>,
}

impl CtxServerHandle {
    /// 等待 server 结束（被 shutdown 或 panic）
    #[allow(dead_code)]
    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(j) = self.join.take() {
            let _ = j.await;
        }
    }

    /// 取得第一个 result 事件
    pub fn first_result(&self) -> Option<CtxEvent> {
        self.events.lock().ok().and_then(|g| g.iter().find(|e| matches!(e, CtxEvent::Result { .. })).cloned())
    }

    /// 是否收到过 status
    pub fn has_status(&self) -> bool {
        self.events.lock().ok().map(|g| g.iter().any(|e| matches!(e, CtxEvent::Status { .. }))).unwrap_or(false)
    }
}

impl Drop for CtxServerHandle {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// 启动监听器
///
/// - `bind_port`: 0 表示随机端口
/// - `expected_test_id`: 期望的 test_id（不匹配返回 400）
pub async fn start_ctx_server(bind_port: u16, expected_test_id: String) -> Result<CtxServerHandle> {
    let events = Arc::new(Mutex::new(Vec::<CtxEvent>::new()));
    let state = ServerState {
        test_id: expected_test_id,
        events: events.clone(),
    };

    let app = Router::new().route("/status", post(handle_status)).route("/result", post(handle_result)).with_state(state);

    let addr = format!("127.0.0.1:{bind_port}");
    let listener = TcpListener::bind(&addr).await.with_context(|| format!("failed to bind {addr}（端口可能已被占用）"))?;
    let actual_port = listener.local_addr()?.port();

    let (tx, rx) = oneshot::channel::<()>();
    let join = tokio::spawn(async move {
        let server = axum::serve(listener, app);
        let shutdown = async {
            let _ = rx.await;
        };
        let _ = server.with_graceful_shutdown(shutdown).await;
    });

    Ok(CtxServerHandle {
        port: actual_port,
        events,
        shutdown: Some(tx),
        join: Some(join),
    })
}

async fn handle_status(State(state): State<ServerState>, Json(payload): Json<Value>) -> Response {
    let test_id = payload.get("test_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if test_id != state.test_id {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "test_id mismatch"}))).into_response();
    }
    let event = CtxEvent::Status {
        test_id,
        stage: payload.get("stage").and_then(|v| v.as_str()).map(String::from),
        percent: payload.get("percent").and_then(|v| v.as_f64()).map(|f| f as f32),
        message: payload.get("message").and_then(|v| v.as_str()).map(String::from),
        raw: payload,
    };
    if let Ok(mut g) = state.events.lock() {
        g.push(event);
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn handle_result(State(state): State<ServerState>, Json(payload): Json<Value>) -> Response {
    let test_id = payload.get("test_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if test_id != state.test_id {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({"error": "test_id mismatch"}))).into_response();
    }
    let ok = payload.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let event = CtxEvent::Result {
        test_id,
        ok,
        message: payload.get("message").and_then(|v| v.as_str()).map(String::from),
        raw: payload,
    };
    if let Ok(mut g) = state.events.lock() {
        g.push(event);
    }
    (StatusCode::OK, Json(serde_json::json!({"ok": true}))).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn status_post_succeeds() {
        let handle = start_ctx_server(0, "test_x".into()).await.unwrap();
        let port = handle.port;
        let client = reqwest_post(port, "/status", json!({"test_id": "test_x", "stage": "step1", "percent": 50.0}));
        let resp = client.await.unwrap();
        assert_eq!(resp, 200);
        assert!(handle.has_status());
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn result_post_succeeds() {
        let handle = start_ctx_server(0, "test_x".into()).await.unwrap();
        let port = handle.port;
        let _ = reqwest_post(port, "/result", json!({"test_id": "test_x", "ok": true})).await.unwrap();
        let first = handle.first_result().unwrap();
        match first {
            CtxEvent::Result { ok, .. } => assert!(ok),
            _ => panic!("expected Result"),
        }
        handle.shutdown().await;
    }

    #[tokio::test]
    async fn mismatched_test_id_returns_400() {
        let handle = start_ctx_server(0, "test_x".into()).await.unwrap();
        let port = handle.port;
        let resp = reqwest_post(port, "/status", json!({"test_id": "WRONG"})).await.unwrap();
        assert_eq!(resp, 400);
        assert!(!handle.has_status());
        handle.shutdown().await;
    }

    /// 简单 POST helper（避免引入 reqwest 依赖）
    async fn reqwest_post(port: u16, path: &str, body: Value) -> Result<u16> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await?;
        let payload = body.to_string();
        let req = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
            payload.len()
        );
        stream.write_all(req.as_bytes()).await?;
        let mut buf = Vec::new();
        stream.read_to_end(&mut buf).await?;
        let resp = String::from_utf8_lossy(&buf);
        // 解析 "HTTP/1.1 NNN"
        let status = resp
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(0);
        Ok(status)
    }
}
