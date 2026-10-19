//! Minimal HTTP/1.1 client for the hub's unix socket.

use crate::{api::ErrorResponse, model::Event, paths};
use bytes::Bytes;
use http::{Method, Request, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper_util::rt::TokioIo;
use serde::{Serialize, de::DeserializeOwned};
use std::{path::PathBuf, time::Duration};
use tokio::net::UnixStream;

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the Liaison hub is not running ({0})")]
    Unavailable(String),
    #[error("hub returned {status}: {message}")]
    Status { status: StatusCode, message: String },
    #[error("request timed out")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

impl ClientError {
    pub fn is_not_found(&self) -> bool {
        matches!(self, ClientError::Status { status, .. } if *status == StatusCode::NOT_FOUND)
    }
}

#[derive(Debug, Clone)]
pub struct HubClient {
    socket: PathBuf,
    timeout: Duration,
}

impl Default for HubClient {
    fn default() -> Self {
        Self::new(paths::socket_path())
    }
}

impl HubClient {
    pub fn new(socket: PathBuf) -> Self {
        Self { socket, timeout: Duration::from_secs(10) }
    }

    /// The default timeout for requests that are not long-polls.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn socket(&self) -> &std::path::Path {
        &self.socket
    }

    pub async fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.request(Method::GET, path, None::<&()>, self.timeout).await
    }

    /// GET with a custom timeout, for long-poll endpoints.
    pub async fn get_long<T: DeserializeOwned>(&self, path: &str, timeout: Duration) -> Result<T, ClientError> {
        self.request(Method::GET, path, None::<&()>, timeout).await
    }

    pub async fn post<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, ClientError> {
        self.request(Method::POST, path, Some(body), self.timeout).await
    }

    pub async fn patch<B: Serialize, T: DeserializeOwned>(&self, path: &str, body: &B) -> Result<T, ClientError> {
        self.request(Method::PATCH, path, Some(body), self.timeout).await
    }

    pub async fn delete<T: DeserializeOwned>(&self, path: &str) -> Result<T, ClientError> {
        self.request(Method::DELETE, path, None::<&()>, self.timeout).await
    }

    pub async fn request<B: Serialize, T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
        timeout: Duration,
    ) -> Result<T, ClientError> {
        let fut = async {
            let mut sender = self.connect().await?;
            let payload = match body {
                Some(b) => serde_json::to_vec(b).map_err(|e| ClientError::Other(e.to_string()))?,
                None => Vec::new(),
            };
            let req = Request::builder()
                .method(method)
                .uri(path)
                .header(http::header::HOST, "liaison")
                .header(http::header::CONTENT_TYPE, "application/json")
                .body(Full::new(Bytes::from(payload)))
                .map_err(|e| ClientError::Other(e.to_string()))?;
            let res = sender.send_request(req).await.map_err(|e| ClientError::Other(e.to_string()))?;
            let status = res.status();
            let bytes = res
                .into_body()
                .collect()
                .await
                .map_err(|e| ClientError::Other(e.to_string()))?
                .to_bytes();
            if !status.is_success() {
                let message = serde_json::from_slice::<ErrorResponse>(&bytes)
                    .map(|e| e.error)
                    .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
                return Err(ClientError::Status { status, message });
            }
            let bytes = if bytes.is_empty() { Bytes::from_static(b"null") } else { bytes };
            serde_json::from_slice(&bytes).map_err(|e| ClientError::Other(format!("bad response: {e}")))
        };
        tokio::time::timeout(timeout, fut).await.map_err(|_| ClientError::Timeout)?
    }

    /// Follows `GET /v1/events` and forwards every event to `on_event` until the stream ends.
    pub async fn follow_events(&self, mut on_event: impl FnMut(Event)) -> Result<(), ClientError> {
        let mut sender = self.connect().await?;
        let req = Request::builder()
            .method(Method::GET)
            .uri("/v1/events")
            .header(http::header::HOST, "liaison")
            .body(Full::new(Bytes::new()))
            .map_err(|e| ClientError::Other(e.to_string()))?;
        let res = sender.send_request(req).await.map_err(|e| ClientError::Other(e.to_string()))?;
        if !res.status().is_success() {
            return Err(ClientError::Status { status: res.status(), message: "event stream refused".into() });
        }
        let mut body = res.into_body();
        let mut buf: Vec<u8> = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|e| ClientError::Other(e.to_string()))?;
            let Some(data) = frame.data_ref() else { continue };
            buf.extend_from_slice(data);
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buf.drain(..=pos).collect();
                let line = &line[..line.len() - 1];
                if line.iter().all(u8::is_ascii_whitespace) {
                    continue; // keep-alive
                }
                if let Ok(event) = serde_json::from_slice::<Event>(line) {
                    on_event(event);
                }
            }
        }
        Ok(())
    }

    async fn connect(&self) -> Result<hyper::client::conn::http1::SendRequest<Full<Bytes>>, ClientError> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .map_err(|e| ClientError::Unavailable(format!("{}: {e}", self.socket.display())))?;
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| ClientError::Other(e.to_string()))?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        Ok(sender)
    }
}

/// Percent-encodes a value for use in a query string.
pub fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
