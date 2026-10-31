//! 极简 HTTP mock 服务，仅供单元测试使用（不引入额外依赖）。
//!
//! 每个 TCP 连接处理一个请求：读取请求行 + 头 + 按 `Content-Length` 读取 body，
//! 交给测试提供的 handler 决定响应，并记录请求供断言。
//! 响应统一带 `Connection: close`，让 reqwest 每次新建连接，避免复用带来的时序问题。

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// mock 服务记录到的一次请求。
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl RecordedRequest {
    /// 读取头（名称大小写不敏感）。
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// body 按 UTF-8 解释（multipart / JSON 断言用）。
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// mock 服务返回的响应。
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

impl MockResponse {
    /// 200 + JSON 响应。
    pub fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            content_type: "application/json; charset=utf-8".to_string(),
            body: body.into(),
        }
    }

    /// 覆盖 HTTP 状态码。
    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }
}

/// 跑在后台的 mock HTTP 服务；drop 时自动停止。
pub struct MockHttpServer {
    addr: SocketAddr,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    handle: JoinHandle<()>,
}

impl MockHttpServer {
    /// 启动 mock 服务；`handler` 针对每个请求返回响应（需是线程安全的）。
    pub async fn start<F>(handler: F) -> Self
    where
        F: Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static,
    {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("绑定 mock HTTP 端口失败");
        let addr = listener.local_addr().expect("获取 mock HTTP 端口失败");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);

        let requests_server = requests.clone();
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                let requests = requests_server.clone();
                tokio::spawn(async move {
                    handle_connection(stream, handler, requests).await;
                });
            }
        });

        Self {
            addr,
            requests,
            handle,
        }
    }

    /// mock 服务根地址，如 `http://127.0.0.1:54321`。
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// 已记录的请求（按完成顺序）。
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().expect("mock requests mutex").clone()
    }

    /// 已处理的请求数量。
    pub fn request_count(&self) -> usize {
        self.requests.lock().expect("mock requests mutex").len()
    }
}

impl Drop for MockHttpServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    handler: Arc<dyn Fn(&RecordedRequest) -> MockResponse + Send + Sync>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
) {
    let Some(request) = read_request(&mut stream).await else {
        return;
    };
    let response = handler(&request);
    requests.lock().expect("mock requests mutex").push(request);
    write_response(&mut stream, &response).await;
}

async fn read_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];

    let header_end = loop {
        if let Some(position) = find_header_end(&buffer) {
            break position;
        }
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > 4 * 1024 * 1024 {
            return None;
        }
    };

    let header_text = String::from_utf8_lossy(&buffer[..header_end]).into_owned();
    let mut lines = header_text.split("\r\n");
    let request_line = lines.next()?;
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next()?.to_string();
    let path = request_parts.next()?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
        }
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }
    let body_end = (body_start + content_length).min(buffer.len());
    let body = buffer[body_start..body_end].to_vec();

    Some(RecordedRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn write_response(stream: &mut TcpStream, response: &MockResponse) {
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.status,
        reason,
        response.content_type,
        response.body.len()
    );

    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(response.body.as_bytes()).await;
    let _ = stream.shutdown().await;
}
