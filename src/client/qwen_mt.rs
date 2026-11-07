//! 文本翻译客户端（`qwen-mt-flash`，百炼 OpenAI 兼容端点）。
//!
//! 协议实测与字段约束见 `docs/architecture.md` 第 15 节（翻译模式），关键约束：
//! - `translation_options` 必须携带，否则模型会退化成普通对话模型；
//! - 语言代码直接透传 ISO（`zh`/`en`/...），不需要映射成英文名；
//! - 译文在 `choices[0].message.content`。
//!
//! 本模块只负责一次请求、错误归类与一次重试；批次、队列与句终屏障由翻译 worker 负责。

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// 建立连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 单次翻译请求超时（实测短句 0.2–0.3s，留足余量）。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// 网络错误 / 5xx 的重试间隔。
const RETRY_BACKOFF: Duration = Duration::from_millis(300);

/// 服务端限流（429）的重试间隔。
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(1);

/// 最多尝试次数（首次 + 一次重试），避免持续失败时一直拖住字幕。
const MAX_ATTEMPTS: usize = 2;

/// 一次翻译请求。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslateRequest {
    pub text: String,
    /// 源语言：ISO 代码或 `auto`（自动检测）。
    pub source_language: String,
    /// 目标语言：ISO 代码。
    pub target_language: String,
}

impl TranslateRequest {
    /// 构造请求（文本去掉首尾空白；空文本仍由 [`QwenMtClient::translate`] 拒绝）。
    pub fn new(
        text: impl Into<String>,
        source_language: impl Into<String>,
        target_language: impl Into<String>,
    ) -> Self {
        Self {
            text: text.into().trim().to_string(),
            source_language: source_language.into(),
            target_language: target_language.into(),
        }
    }
}

/// `qwen-mt` 翻译客户端（克隆便宜：内部 `reqwest::Client` 共享连接池）。
#[derive(Clone)]
pub struct QwenMtClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
    url: String,
}

impl QwenMtClient {
    /// `url` 是完整的兼容端点地址（见 `AppConfig::translate_chat_url`）。
    pub fn new(
        api_key: impl Into<String>,
        model: impl Into<String>,
        url: impl Into<String>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .user_agent(concat!("kaols/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("创建翻译 HTTP 客户端失败")?;
        Ok(Self {
            http,
            api_key: api_key.into(),
            model: model.into(),
            url: url.into(),
        })
    }

    /// 翻译一段文本，返回清理过首尾空白的译文。
    ///
    /// 网络错误、429、5xx 会重试一次；4xx（参数/鉴权错误）立即失败，
    /// 错误文本带上服务端返回的 `code` 与 `message`。
    pub async fn translate(&self, request: &TranslateRequest) -> Result<String> {
        if request.text.trim().is_empty() {
            bail!("翻译请求文本为空");
        }
        let body = ChatRequest {
            model: &self.model,
            messages: [ChatMessage {
                role: "user",
                content: &request.text,
            }],
            translation_options: TranslationOptions {
                source_lang: &request.source_language,
                target_lang: &request.target_language,
            },
        };

        let mut last_error: Option<String> = None;
        for attempt in 0..MAX_ATTEMPTS {
            match self.send(&body).await {
                Ok(text) => return Ok(text),
                Err(error) => {
                    let can_retry = error.retryable && attempt + 1 < MAX_ATTEMPTS;
                    last_error = Some(error.message);
                    if !can_retry {
                        break;
                    }
                    tokio::time::sleep(error.backoff).await;
                }
            }
        }
        Err(anyhow::anyhow!(
            last_error.unwrap_or_else(|| "翻译失败".to_string())
        ))
    }

    async fn send(&self, body: &ChatRequest<'_>) -> std::result::Result<String, AttemptError> {
        let response = self
            .http
            .post(&self.url)
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(|error| {
                AttemptError::retryable(format!("翻译请求网络错误: {error}"), RETRY_BACKOFF)
            })?;

        let status = response.status();
        let text = response.text().await.map_err(|error| {
            AttemptError::retryable(format!("读取翻译响应失败: {error}"), RETRY_BACKOFF)
        })?;

        if !status.is_success() {
            let retryable = status.as_u16() == 429 || status.is_server_error();
            let backoff = if status.as_u16() == 429 {
                RATE_LIMIT_BACKOFF
            } else {
                RETRY_BACKOFF
            };
            return Err(AttemptError {
                message: format!(
                    "翻译接口返回 HTTP {}: {}",
                    status.as_u16(),
                    parse_error_detail(&text)
                ),
                retryable,
                backoff,
            });
        }

        let parsed: ChatResponse = serde_json::from_str(&text).map_err(|error| {
            AttemptError::fatal(format!(
                "解析翻译响应失败（HTTP {}）: {error}; 响应: {}",
                status.as_u16(),
                snippet(&text)
            ))
        })?;
        parsed
            .choices
            .into_iter()
            .next()
            .and_then(|choice| choice.message.content)
            .map(|content| content.trim().to_string())
            .filter(|content| !content.is_empty())
            .ok_or_else(|| AttemptError::fatal(format!("翻译响应缺少译文内容: {}", snippet(&text))))
    }
}

/// 内部尝试错误：区分「可重试」与「立即失败」。
struct AttemptError {
    message: String,
    retryable: bool,
    backoff: Duration,
}

impl AttemptError {
    fn retryable(message: impl Into<String>, backoff: Duration) -> Self {
        Self {
            message: message.into(),
            retryable: true,
            backoff,
        }
    }

    fn fatal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
            backoff: Duration::ZERO,
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [ChatMessage<'a>; 1],
    translation_options: TranslationOptions<'a>,
}

#[derive(Serialize)]
struct ChatMessage<'a> {
    role: &'static str,
    content: &'a str,
}

/// 翻译参数：缺失时模型会退化成普通对话模型，因此始终显式发送。
#[derive(Serialize)]
struct TranslationOptions<'a> {
    source_lang: &'a str,
    target_lang: &'a str,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessageContent,
}

#[derive(Deserialize)]
struct ChatMessageContent {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: Option<ApiError>,
}

#[derive(Deserialize)]
struct ApiError {
    code: Option<String>,
    message: Option<String>,
}

/// 从错误响应里提取 `code: message`；解析失败时回退截断后的原文。
fn parse_error_detail(body: &str) -> String {
    if let Ok(ErrorEnvelope { error: Some(error) }) = serde_json::from_str::<ErrorEnvelope>(body) {
        let code = error.code.unwrap_or_default();
        let message = error.message.unwrap_or_default();
        return match (code.is_empty(), message.is_empty()) {
            (false, false) => format!("{code}: {message}"),
            (false, true) => code,
            (true, false) => message,
            (true, true) => snippet(body),
        };
    }
    snippet(body)
}

/// 截断响应正文，避免把整段 HTML/JSON 塞进错误消息。
fn snippet(body: &str) -> String {
    let compact = body.trim().replace('\n', " ");
    let mut snippet: String = compact.chars().take(200).collect();
    if compact.chars().count() > 200 {
        snippet.push('…');
    }
    snippet
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_client(server: &MockHttpServer) -> QwenMtClient {
        QwenMtClient::new(
            "sk-test",
            "qwen-mt-flash",
            format!("{}/compatible-mode/v1/chat/completions", server.base_url()),
        )
        .unwrap()
    }

    fn request(text: &str, source: &str, target: &str) -> TranslateRequest {
        TranslateRequest::new(text, source, target)
    }

    #[tokio::test]
    async fn translate_sends_translation_options_and_returns_text() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"choices":[{"message":{"role":"assistant","content":" Hello, world! "}}]}"#,
            )
        })
        .await;
        let client = test_client(&server);

        let text = client
            .translate(&request("你好，世界！", "zh", "en"))
            .await
            .unwrap();
        assert_eq!(text, "Hello, world!", "应去掉译文首尾空白");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let recorded = &requests[0];
        assert_eq!(recorded.method, "POST");
        assert_eq!(recorded.path, "/compatible-mode/v1/chat/completions");
        assert_eq!(
            recorded.header("authorization"),
            Some("Bearer sk-test"),
            "应带 Bearer 鉴权"
        );

        let body: serde_json::Value = serde_json::from_str(&recorded.body_text()).unwrap();
        assert_eq!(body["model"], "qwen-mt-flash");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["messages"][0]["content"], "你好，世界！");
        assert_eq!(
            body["translation_options"]["source_lang"], "zh",
            "必须携带 translation_options，否则会退化成对话模型"
        );
        assert_eq!(body["translation_options"]["target_lang"], "en");
    }

    #[tokio::test]
    async fn translate_rejects_empty_text_without_request() {
        let server = MockHttpServer::start(|_request| MockResponse::json("{}")).await;
        let client = test_client(&server);

        let err = client
            .translate(&request("   ", "zh", "en"))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("翻译请求文本为空"), "{err:#}");
        assert_eq!(server.request_count(), 0, "空文本不应发请求");
    }

    #[tokio::test]
    async fn translate_surfaces_api_error_without_retry() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"error":{"code":"invalid_parameter_error","message":"暂时不支持当前设置的语种！"}}"#,
            )
            .status(400)
        })
        .await;
        let client = test_client(&server);

        let err = client
            .translate(&request("你好", "zh", "xx"))
            .await
            .unwrap_err();
        let message = format!("{err:#}");
        assert!(message.contains("HTTP 400"), "{message}");
        assert!(message.contains("invalid_parameter_error"), "{message}");
        assert!(message.contains("暂时不支持当前设置的语种"), "{message}");
        assert_eq!(server.request_count(), 1, "4xx 不应重试");
    }

    #[tokio::test]
    async fn translate_retries_once_on_server_error() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_for_handler = calls.clone();
        let server = MockHttpServer::start(move |_request| {
            let attempt = calls_for_handler.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                MockResponse::json(
                    r#"{"error":{"code":"internal_error","message":"service busy"}}"#,
                )
                .status(500)
            } else {
                MockResponse::json(r#"{"choices":[{"message":{"content":"ok"}}]}"#)
            }
        })
        .await;
        let client = test_client(&server);

        let text = client
            .translate(&request("你好", "zh", "en"))
            .await
            .unwrap();
        assert_eq!(text, "ok");
        assert_eq!(server.request_count(), 2, "5xx 应重试一次后成功");
    }

    #[tokio::test]
    async fn translate_reports_malformed_response() {
        let server = MockHttpServer::start(|_request| MockResponse::json("not json")).await;
        let client = test_client(&server);

        let err = client
            .translate(&request("你好", "zh", "en"))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("解析翻译响应失败"), "{err:#}");
        assert_eq!(server.request_count(), 1, "响应解析失败不应重试");
    }

    #[tokio::test]
    async fn translate_reports_missing_content() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"choices":[{"message":{"content":"   "}}]}"#)
        })
        .await;
        let client = test_client(&server);

        let err = client
            .translate(&request("你好", "zh", "en"))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("缺少译文内容"), "{err:#}");
    }
}
