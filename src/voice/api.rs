//! 百炼自定义音色（声音复刻）HTTP API 客户端。
//!
//! 不依赖任何 SDK，直接调 DashScope 的 HTTP 接口（基址 `https://{host}/api/v1`）：
//!
//! - 样例音频上传 / 取下载地址 / 删除：`POST|GET|DELETE /files[/{file_id}]`
//! - 音色复刻 / 查询 / 列表 / 删除：`POST /services/audio/tts/customization`，
//!   通过请求体 `input.action` 区分动作（`create_voice` / `query_voice` /
//!   `list_voice` / `delete_voice`），`model` 固定为 `voice-enrollment`。
//!
//! 复刻接口需要「能被服务端立即下载的音频」：本项目走文件服务 URL
//! （上传样例音频 → 用 file_id 换 24h 有效的临时下载 URL → 创建音色 → 轮询到就绪），
//! 官方推荐的 `audio` 参数也支持公网 URL 或 Base64 Data URI。
//!
//! 注意两种响应外壳：`/files*` 用 `data`，`/services/.../customization` 用 `output`。

use anyhow::{bail, Context, Result};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, RequestBuilder, Response};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::{Duration, Instant};

/// 音色复刻服务使用的固定模型名（不是 TTS 模型）。
pub const VOICE_ENROLLMENT_MODEL: &str = "voice-enrollment";

/// 百炼 HTTP 接口返回的业务错误（保留状态码与错误码，便于按类型判断）。
#[derive(Debug)]
pub struct ApiError {
    pub action: String,
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// 资源不存在：音色/文件被删除或从未创建。
    ///
    /// 实测对不存在的 voice_id 调 `query_voice` / `delete_voice` 返回
    /// HTTP 400 + `BadRequest.ResourceNotExist`，可据此判定缓存失效。
    pub fn is_resource_missing(&self) -> bool {
        self.status == 404
            || self.code == "BadRequest.ResourceNotExist"
            || self.message.to_ascii_lowercase().contains("not exist")
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "百炼「{}」失败（HTTP {} {}): {}",
            self.action, self.status, self.code, self.message
        )
    }
}

impl std::error::Error for ApiError {}

/// 判断错误链中是否包含「资源不存在」的百炼业务错误。
pub fn is_resource_missing_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|cause| cause.downcast_ref::<ApiError>())
        .map(ApiError::is_resource_missing)
        .unwrap_or(false)
}

/// 列表接口每页条数。
const LIST_PAGE_SIZE: usize = 50;
/// 列表接口最多翻页数（防御异常响应导致死循环）。
const LIST_MAX_PAGES: usize = 20;
/// 轮询部署状态的间隔。
const READY_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// 百炼账号下的一个自定义（复刻）音色。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteVoice {
    pub voice_id: String,
    pub target_model: String,
    pub status: String,
    pub created_time: String,
}

impl RemoteVoice {
    /// 该音色是否已经部署完成、可用于合成。
    pub fn is_ready(&self) -> bool {
        is_ready_status(&self.status)
    }
}

/// `list_voice` 的完整结果（含是否确认枚举完整）。
#[derive(Debug, Clone, Default)]
pub struct RemoteVoiceList {
    pub voices: Vec<RemoteVoice>,
    /// 服务端返回的 `total_count`（若返回）。
    pub total_count: Option<u64>,
    /// 是否确认已枚举全部音色。
    ///
    /// 只有为 `true` 时才可据此清理本地缓存或执行「删除全部」，
    /// 避免分页异常/响应结构异常时误删。
    pub complete: bool,
}

/// 已上传到百炼文件服务的样例音频。
#[derive(Debug, Clone)]
pub struct UploadedAudio {
    pub file_id: String,
    pub name: String,
}

/// `query_voice` 返回的音色详情。
#[derive(Debug, Clone)]
pub struct VoiceDetail {
    pub status: String,
    /// 原始 output，便于在失败时打印服务端信息。
    pub raw: Value,
}

/// 百炼自定义音色 API 客户端。
#[derive(Debug, Clone)]
pub struct BailianVoiceApi {
    client: Client,
    api_key: String,
    base_url: String,
}

impl BailianVoiceApi {
    /// `base_url` 形如 `https://dashscope.aliyuncs.com/api/v1`（不带末尾斜杠也可）。
    pub fn new(api_key: impl Into<String>, base_url: impl Into<String>) -> Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .user_agent(concat!("kaols/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("创建百炼 HTTP 客户端失败")?;
        Ok(Self {
            client,
            api_key: api_key.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn authorized(&self, request: RequestBuilder) -> RequestBuilder {
        request.bearer_auth(&self.api_key)
    }

    /// 上传样例音频（multipart，字段名 `files` + `purpose=inference`）。
    pub async fn upload_audio(&self, file_name: &str, bytes: Vec<u8>) -> Result<UploadedAudio> {
        let part = Part::bytes(bytes).file_name(file_name.to_string());
        let form = Form::new().text("purpose", "inference").part("files", part);
        let request = self.authorized(self.client.post(self.url("/files")).multipart(form));

        let response = request.send().await.context("上传样例音频到百炼失败")?;
        let value = parse_response(response, "上传样例音频").await?;
        let payload = payload(&value);

        if let Some(failed) = payload
            .get("failed_uploads")
            .and_then(Value::as_array)
            .filter(|failed| !failed.is_empty())
        {
            bail!("百炼拒绝上传样例音频: {}", failed[0]);
        }

        let uploaded = payload
            .get("uploaded_files")
            .and_then(Value::as_array)
            .and_then(|files| files.first())
            .context("百炼上传接口响应缺少 uploaded_files")?;
        let file_id = uploaded
            .get("file_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .context("百炼上传接口响应缺少 file_id")?;
        let name = uploaded
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(file_name)
            .to_string();

        Ok(UploadedAudio {
            file_id: file_id.to_string(),
            name,
        })
    }

    /// 用 file_id 换取临时下载 URL（只有这种 URL 才能传给复刻接口）。
    pub async fn file_download_url(&self, file_id: &str) -> Result<String> {
        let request = self.authorized(self.client.get(self.url(&format!("/files/{file_id}"))));
        let response = request.send().await.context("查询上传文件下载地址失败")?;
        let value = parse_response(response, "查询上传文件").await?;

        payload(&value)
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_string)
            .with_context(|| format!("百炼文件 {file_id} 响应缺少下载 url"))
    }

    /// 删除上传的文件（复刻完成后清理临时文件，失败可忽略）。
    pub async fn delete_file(&self, file_id: &str) -> Result<()> {
        let request = self.authorized(self.client.delete(self.url(&format!("/files/{file_id}"))));
        let response = request.send().await.context("删除百炼临时文件失败")?;
        parse_response(response, "删除上传文件").await?;
        Ok(())
    }

    /// 向音色复刻服务发一个带 `input.action` 的请求，返回 `output`（或 `data`）对象。
    async fn customization(&self, action: &str, input: Value) -> Result<Value> {
        let body = json!({
            "model": VOICE_ENROLLMENT_MODEL,
            "input": input,
        });
        let request = self.authorized(
            self.client
                .post(self.url("/services/audio/tts/customization"))
                .json(&body),
        );
        let response = request
            .send()
            .await
            .with_context(|| format!("请求百炼音色复刻服务失败（{action}）"))?;
        let value = parse_response(response, action).await?;
        Ok(payload(&value).clone())
    }

    /// 创建复刻音色，返回 voice_id（此时可能仍在部署中）。
    ///
    /// `audio_source` 可以是公网/文件服务 URL（本项目用上传后拿到的临时 URL），
    /// 也可以是 Base64 Data URI；使用官方推荐的 `audio` 参数（旧的 `url` 参数将废弃）。
    pub async fn create_voice(
        &self,
        target_model: &str,
        prefix: &str,
        audio_source: &str,
        language: Option<&str>,
    ) -> Result<String> {
        let mut input = json!({
            "action": "create_voice",
            "target_model": target_model,
            "prefix": prefix,
            "audio": audio_source,
        });
        if let Some(language) = language.map(str::trim).filter(|lang| !lang.is_empty()) {
            input["language_hints"] = json!([language]);
        }

        let output = self
            .customization("创建复刻音色", input)
            .await
            .with_context(|| format!("创建复刻音色失败（模型 {target_model}，前缀 {prefix}）"))?;

        output
            .get("voice_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .context("百炼创建音色响应缺少 voice_id")
    }

    /// 查询单个音色的部署状态。
    pub async fn query_voice(&self, voice_id: &str) -> Result<VoiceDetail> {
        let output = self
            .customization(
                "查询音色状态",
                json!({
                    "action": "query_voice",
                    "voice_id": voice_id,
                }),
            )
            .await
            .with_context(|| format!("查询音色 {voice_id} 状态失败"))?;
        Ok(VoiceDetail {
            status: output
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            raw: output,
        })
    }

    /// 轮询直到音色部署完成；超时后打印警告并继续（音色可能已可用）。
    pub async fn wait_until_ready(&self, voice_id: &str, timeout: Duration) -> Result<()> {
        self.wait_until_ready_with(voice_id, timeout, READY_POLL_INTERVAL)
            .await
    }

    /// 可指定轮询间隔的版本（测试用）。
    pub async fn wait_until_ready_with(
        &self,
        voice_id: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<()> {
        let started = Instant::now();

        loop {
            let detail = self.query_voice(voice_id).await?;
            let last_status = detail.status;

            if is_ready_status(&last_status) {
                return Ok(());
            }
            if is_failed_status(&last_status) {
                bail!(
                    "百炼音色 {voice_id} 复刻失败（status={last_status}）: {}",
                    detail.raw
                );
            }
            if started.elapsed() >= timeout {
                log::warn!(
                    "⚠️ 音色 {voice_id} 在 {}s 内仍未就绪（最后状态: {last_status:?}），先继续使用",
                    timeout.as_secs()
                );
                return Ok(());
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// 列出账号下全部自定义音色（自动翻页）。
    ///
    /// 返回的 [`RemoteVoiceList::complete`] 表示是否确认取全：
    /// 优先用响应里的 `total_count` 判断，没有该字段时退化为「短页即末页」。
    /// 调用方只有在 `complete == true` 时才应据此清理本地缓存或执行「删除全部」。
    ///
    /// 完整性判定会去重（按 voice_id），并对异常响应保守处理：
    /// `voice_list` 缺失/非数组、空页但未达到 `total_count`、重复页等一律判为不完整。
    pub async fn list_voices(&self) -> Result<RemoteVoiceList> {
        let mut voices: Vec<RemoteVoice> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut total_count: Option<u64> = None;
        let mut complete = false;

        for page_index in 0..LIST_MAX_PAGES {
            let output = self
                .customization(
                    "列出音色",
                    json!({
                        "action": "list_voice",
                        "page_index": page_index,
                        "page_size": LIST_PAGE_SIZE,
                    }),
                )
                .await
                .with_context(|| format!("列出百炼自定义音色失败（第 {page_index} 页）"))?;

            if let Some(total) = output.get("total_count").and_then(Value::as_u64) {
                total_count = Some(total);
            }

            // 响应里没有 voice_list、或它不是数组（异常 200）时视为「无法确认完整性」
            let Some(page) = output.get("voice_list").and_then(Value::as_array) else {
                break;
            };

            if page.is_empty() {
                // 空页一般表示结束；但若已知 total_count 且还没取够，说明枚举不完整
                complete = match total_count {
                    Some(total) => voices.len() as u64 >= total,
                    None => true,
                };
                break;
            }

            // 按 voice_id 去重，避免服务端忽略 page_index 时重复项把数量"灌"到 total_count
            let mut new_items = 0usize;
            for item in page {
                let voice = parse_remote_voice(item);
                let key = if voice.voice_id.is_empty() {
                    format!("__empty_{}", voices.len())
                } else {
                    voice.voice_id.clone()
                };
                if seen.insert(key) {
                    voices.push(voice);
                    new_items += 1;
                }
            }
            if new_items == 0 {
                // 整页都是重复项：无法继续推进，判为不完整
                break;
            }

            match total_count {
                Some(total) if voices.len() as u64 >= total => {
                    complete = true;
                    break;
                }
                Some(_) => {}
                None if page.len() < LIST_PAGE_SIZE => {
                    complete = true;
                    break;
                }
                None => {}
            }
        }

        Ok(RemoteVoiceList {
            voices,
            total_count,
            complete,
        })
    }

    /// 删除一个自定义音色。
    pub async fn delete_voice(&self, voice_id: &str) -> Result<()> {
        self.customization(
            "删除音色",
            json!({
                "action": "delete_voice",
                "voice_id": voice_id,
            }),
        )
        .await
        .with_context(|| format!("删除音色 {voice_id} 失败"))?;
        Ok(())
    }
}

/// 服务端音色状态是否表示「已就绪」。
pub fn is_ready_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_uppercase().as_str(),
        "OK" | "READY" | "SUCCESS" | "COMPLETED" | "DEPLOYED" | "AVAILABLE"
    )
}

/// 服务端音色状态是否表示「失败」。
pub fn is_failed_status(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_uppercase().as_str(),
        "FAILED" | "FAIL" | "ERROR"
    )
}

/// 兼容 `output`（服务接口）与 `data`（文件接口）两种响应包裹。
fn payload(value: &Value) -> &Value {
    value
        .get("output")
        .filter(|node| node.is_object())
        .or_else(|| value.get("data").filter(|node| node.is_object()))
        .unwrap_or(value)
}

/// 解析响应：非 2xx 时返回带状态码/错误码的 [`ApiError`]。
async fn parse_response(response: Response, action: &str) -> Result<Value> {
    let status = response.status();
    let body = response
        .text()
        .await
        .with_context(|| format!("读取百炼「{action}」响应失败"))?;

    let value: Value = serde_json::from_str(&body).unwrap_or(Value::Null);

    if !status.is_success() {
        let code = value
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("UnknownError");
        let message = value
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_else(|| body.trim());
        return Err(ApiError {
            action: action.to_string(),
            status: status.as_u16(),
            code: code.to_string(),
            message: message.to_string(),
        }
        .into());
    }

    Ok(value)
}

fn parse_remote_voice(item: &Value) -> RemoteVoice {
    let text = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| item.get(*key).and_then(Value::as_str))
            .unwrap_or_default()
            .to_string()
    };

    RemoteVoice {
        voice_id: text(&["voice_id", "id"]),
        target_model: text(&["target_model", "model"]),
        status: text(&["status"]),
        created_time: text(&["created_time", "gmt_create", "gmt_modified"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse, RecordedRequest};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn test_api(server: &MockHttpServer) -> BailianVoiceApi {
        BailianVoiceApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap()
    }

    #[tokio::test]
    async fn upload_audio_sends_multipart_and_parses_file_id() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"data":{"uploaded_files":[{"name":"kaoet.wav","file_id":"file-1"}],"failed_uploads":[]}}"#,
            )
        })
        .await;
        let api = test_api(&server);

        let uploaded = api
            .upload_audio("kaoet.wav", b"RIFF....WAVE".to_vec())
            .await
            .unwrap();

        assert_eq!(uploaded.file_id, "file-1");
        assert_eq!(uploaded.name, "kaoet.wav");

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/api/v1/files");
        assert_eq!(
            request.header("authorization"),
            Some("Bearer sk-test"),
            "必须携带 Bearer 鉴权头"
        );
        assert!(request
            .header("content-type")
            .unwrap_or_default()
            .starts_with("multipart/form-data"));
        let body = request.body_text();
        assert!(body.contains("name=\"files\""), "文件字段名应为 files");
        assert!(body.contains("kaoet.wav"), "multipart 应带文件名");
        assert!(
            body.contains("name=\"purpose\"") && body.contains("inference"),
            "multipart 应带 purpose=inference 字段: {body}"
        );
        assert!(body.contains("RIFF....WAVE"), "multipart 应带音频内容");
    }

    #[tokio::test]
    async fn rejects_failed_upload() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"data":{"uploaded_files":[],"failed_uploads":[{"name":"bad.wav","error":"invalid format"}]}}"#,
            )
        })
        .await;
        let api = test_api(&server);

        let err = api.upload_audio("bad.wav", vec![0u8; 8]).await.unwrap_err();
        assert!(format!("{err:#}").contains("invalid format"), "{err:#}");
    }

    #[tokio::test]
    async fn file_download_url_and_delete_use_file_endpoint() {
        let server = MockHttpServer::start(|request| match request.method.as_str() {
            "GET" => MockResponse::json(
                r#"{"data":{"file_id":"file-9","url":"https://oss/x.wav?sig=1"}}"#,
            ),
            "DELETE" => MockResponse::json(r#"{"request_id":"r1"}"#),
            _ => MockResponse::json("{}").status(405),
        })
        .await;
        let api = test_api(&server);

        let url = api.file_download_url("file-9").await.unwrap();
        assert_eq!(url, "https://oss/x.wav?sig=1");
        api.delete_file("file-9").await.unwrap();

        let requests = server.requests();
        assert_eq!(requests[0].method, "GET");
        assert_eq!(requests[0].path, "/api/v1/files/file-9");
        assert_eq!(requests[1].method, "DELETE");
        assert_eq!(requests[1].path, "/api/v1/files/file-9");
    }

    #[tokio::test]
    async fn create_voice_posts_enrollment_body() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"output":{"voice_id":"qwen-audio-3.1-tts-flash-kaoet1a2b-abc","preview_audio":{}},"usage":{"count":1}}"#,
            )
        })
        .await;
        let api = test_api(&server);

        let voice_id = api
            .create_voice(
                "qwen-audio-3.1-tts-flash",
                "kaoet1a2b",
                "https://oss/x.wav",
                Some("zh"),
            )
            .await
            .unwrap();
        assert_eq!(voice_id, "qwen-audio-3.1-tts-flash-kaoet1a2b-abc");

        let requests = server.requests();
        assert_eq!(requests[0].path, "/api/v1/services/audio/tts/customization");
        let body: Value = serde_json::from_str(&requests[0].body_text()).unwrap();
        assert_eq!(body["model"], "voice-enrollment");
        assert_eq!(body["input"]["action"], "create_voice");
        assert_eq!(body["input"]["target_model"], "qwen-audio-3.1-tts-flash");
        assert_eq!(body["input"]["prefix"], "kaoet1a2b");
        assert_eq!(
            body["input"]["audio"], "https://oss/x.wav",
            "应使用官方推荐的 audio 参数（url 将废弃）"
        );
        assert!(body["input"].get("url").is_none(), "不应再传旧的 url 参数");
        assert_eq!(body["input"]["language_hints"][0], "zh");
    }

    #[tokio::test]
    async fn create_voice_omits_language_hints_when_not_configured() {
        let server =
            MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{"voice_id":"v1"}}"#))
                .await;
        let api = test_api(&server);

        api.create_voice("model", "prefix12", "https://oss/x.wav", None)
            .await
            .unwrap();

        let body: Value = serde_json::from_str(&server.requests()[0].body_text()).unwrap();
        assert!(body["input"].get("language_hints").is_none());
    }

    #[tokio::test]
    async fn api_error_reports_code_and_message() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"code":"InvalidParameter","message":"prefix should be english letter and number"}"#,
            )
            .status(400)
        })
        .await;
        let api = test_api(&server);

        let err = api
            .create_voice("model", "bad-prefix", "https://oss/x.wav", None)
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("HTTP 400"), "{text}");
        assert!(text.contains("InvalidParameter"), "{text}");
        assert!(text.contains("english letter and number"), "{text}");
        assert!(
            !is_resource_missing_error(&err),
            "普通参数错误不应被判定为资源不存在"
        );
    }

    /// 实测：对不存在的 voice_id 调 query_voice 返回 400 + BadRequest.ResourceNotExist
    #[tokio::test]
    async fn query_missing_voice_is_detected_as_resource_missing() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"code":"BadRequest.ResourceNotExist","message":"The Required resource not exist"}"#,
            )
            .status(400)
        })
        .await;
        let api = test_api(&server);

        let err = api.query_voice("v-missing").await.unwrap_err();
        assert!(
            is_resource_missing_error(&err),
            "应识别为资源不存在: {err:#}"
        );
    }

    #[tokio::test]
    async fn network_error_is_not_treated_as_missing_voice() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"code":"InternalError","message":"boom"}"#).status(500)
        })
        .await;
        let api = test_api(&server);

        let err = api.query_voice("v1").await.unwrap_err();
        assert!(
            !is_resource_missing_error(&err),
            "服务端错误不应误判: {err:#}"
        );
    }

    #[tokio::test]
    async fn wait_until_ready_polls_until_ok() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_server = calls.clone();
        let server = MockHttpServer::start(move |request| {
            let body: Value = serde_json::from_str(&request.body_text()).unwrap();
            assert_eq!(body["input"]["action"], "query_voice");
            match calls_server.fetch_add(1, Ordering::SeqCst) {
                0 => MockResponse::json(r#"{"output":{"status":"DEPLOYING"}}"#),
                1 => MockResponse::json(r#"{"output":{"status":"DEPLOYING"}}"#),
                _ => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            }
        })
        .await;
        let api = test_api(&server);

        api.wait_until_ready_with("v1", Duration::from_secs(2), Duration::from_millis(5))
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn wait_until_ready_fails_fast_on_failed_status() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"output":{"status":"FAILED","reason":"bad audio"}}"#)
        })
        .await;
        let api = test_api(&server);

        let err = api
            .wait_until_ready_with("v1", Duration::from_secs(2), Duration::from_millis(5))
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("复刻失败"), "{err:#}");
    }

    #[tokio::test]
    async fn list_voices_paginates_until_short_page() {
        let server = MockHttpServer::start(|request| {
            let body: Value = serde_json::from_str(&request.body_text()).unwrap();
            assert_eq!(body["input"]["action"], "list_voice");
            let page_index = body["input"]["page_index"].as_u64().unwrap();
            if page_index == 0 {
                let items: Vec<String> = (0..50)
                    .map(|i| format!(r#"{{"voice_id":"v{i}","status":"OK"}}"#))
                    .collect();
                MockResponse::json(format!(r#"{{"output":{{"voice_list":[{}]}}}}"#, items.join(",")))
            } else {
                MockResponse::json(
                    r#"{"output":{"voice_list":[{"voice_id":"last","target_model":"qwen-audio-3.1-tts-flash","status":"OK"}]}}"#,
                )
            }
        })
        .await;
        let api = test_api(&server);

        let list = api.list_voices().await.unwrap();
        let voices = &list.voices;
        assert_eq!(voices.len(), 51);
        assert_eq!(voices[0].voice_id, "v0");
        assert_eq!(voices[50].voice_id, "last");
        assert_eq!(
            voices[50].target_model, "qwen-audio-3.1-tts-flash",
            "应解析 target_model"
        );
        assert!(list.complete, "短页应判定为枚举完整");
        assert_eq!(server.request_count(), 2);
    }

    /// `page_size` 被服务端截断时，要靠 total_count 继续翻页并正确判断完整性。
    #[tokio::test]
    async fn list_voices_uses_total_count_to_detect_complete_enumeration() {
        let server = MockHttpServer::start(|request| {
            let body: Value = serde_json::from_str(&request.body_text()).unwrap();
            let page_index = body["input"]["page_index"].as_u64().unwrap();
            match page_index {
                0 => MockResponse::json(
                    r#"{"output":{"total_count":3,"voice_list":[{"voice_id":"v0"},{"voice_id":"v1"}]}}"#,
                ),
                1 => MockResponse::json(
                    r#"{"output":{"total_count":3,"voice_list":[{"voice_id":"v2"}]}}"#,
                ),
                _ => MockResponse::json(r#"{"output":{"total_count":3,"voice_list":[]}}"#),
            }
        })
        .await;
        let api = test_api(&server);

        let list = api.list_voices().await.unwrap();
        assert_eq!(list.voices.len(), 3);
        assert_eq!(list.total_count, Some(3));
        assert!(list.complete);
    }

    /// 异常 200（没有 voice_list 字段）时必须标记为「不完整」，否则会误清缓存。
    #[tokio::test]
    async fn list_voices_without_voice_list_is_incomplete() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"output":{"page_index":0,"page_size":50}}"#)
        })
        .await;
        let api = test_api(&server);

        let list = api.list_voices().await.unwrap();
        assert!(list.voices.is_empty());
        assert!(!list.complete, "缺少 voice_list 时不能判定为完整");
    }

    /// voice_list 存在但不是数组（如 null）同样不能当作「空列表且完整」。
    #[tokio::test]
    async fn list_voices_with_non_array_voice_list_is_incomplete() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"output":{"total_count":0,"voice_list":null}}"#)
        })
        .await;
        let api = test_api(&server);

        let list = api.list_voices().await.unwrap();
        assert!(list.voices.is_empty());
        assert!(!list.complete, "voice_list 非数组时不能判定为完整");
    }

    /// 已知 total_count 但提前遇到空页：说明没有取全，不能判为完整。
    #[tokio::test]
    async fn list_voices_empty_page_before_total_is_incomplete() {
        let server = MockHttpServer::start(|request| {
            let body: Value = serde_json::from_str(&request.body_text()).unwrap();
            let page_index = body["input"]["page_index"].as_u64().unwrap();
            if page_index == 0 {
                MockResponse::json(
                    r#"{"output":{"total_count":5,"voice_list":[{"voice_id":"v0"},{"voice_id":"v1"}]}}"#,
                )
            } else {
                MockResponse::json(r#"{"output":{"total_count":5,"voice_list":[]}}"#)
            }
        })
        .await;
        let api = test_api(&server);

        let list = api.list_voices().await.unwrap();
        assert_eq!(list.voices.len(), 2);
        assert_eq!(list.total_count, Some(5));
        assert!(!list.complete, "未达到 total_count 时不能判定为完整");
    }

    /// 服务端重复返回同一页时要按 voice_id 去重，且不能把重复项当完整。
    #[tokio::test]
    async fn list_voices_repeated_pages_are_incomplete() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"output":{"total_count":5,"voice_list":[{"voice_id":"v0"},{"voice_id":"v1"}]}}"#,
            )
        })
        .await;
        let api = test_api(&server);

        let list = api.list_voices().await.unwrap();
        assert_eq!(list.voices.len(), 2, "重复项应被去重");
        assert!(!list.complete, "重复页无法取全时不能判定为完整");
        assert_eq!(server.request_count(), 2, "发现无新增项后应立即停止翻页");
    }

    #[tokio::test]
    async fn delete_voice_posts_delete_action() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;
        let api = test_api(&server);

        api.delete_voice("voice-1").await.unwrap();

        let body: Value = serde_json::from_str(&server.requests()[0].body_text()).unwrap();
        assert_eq!(body["input"]["action"], "delete_voice");
        assert_eq!(body["input"]["voice_id"], "voice-1");
    }

    #[test]
    fn status_predicates_cover_bailian_values() {
        for status in ["OK", "ok", "READY", "SUCCESS", "COMPLETED", "deployed"] {
            assert!(is_ready_status(status), "{status} 应视为就绪");
        }
        for status in ["FAILED", "fail", "ERROR"] {
            assert!(is_failed_status(status), "{status} 应视为失败");
        }
        assert!(!is_ready_status("DEPLOYING"));
        assert!(!is_failed_status("DEPLOYING"));
        assert!(!is_ready_status(""));
    }

    #[test]
    fn payload_accepts_output_and_data_wrappers() {
        assert_eq!(payload(&json!({"output":{"a":1}}))["a"], 1);
        assert_eq!(payload(&json!({"data":{"b":2}}))["b"], 2);
        assert_eq!(payload(&json!({"c":3}))["c"], 3);
        // output 为 null 时应回退到原始响应
        assert_eq!(payload(&json!({"output": null, "c": 3}))["c"], 3);
    }

    #[test]
    fn parses_remote_voice_fields_from_list_response() {
        let item = json!({
            "voice_id": "qwen-audio-3.1-tts-flash-kaoet1a2b-bd956632b6054ecc88770b4abe741dfc",
            "target_model": "qwen-audio-3.1-tts-flash",
            "status": "OK",
            "gmt_create": "2026-10-31 19:39:49"
        });
        let voice = parse_remote_voice(&item);
        assert_eq!(
            voice.voice_id,
            "qwen-audio-3.1-tts-flash-kaoet1a2b-bd956632b6054ecc88770b4abe741dfc"
        );
        assert_eq!(voice.target_model, "qwen-audio-3.1-tts-flash");
        assert_eq!(voice.status, "OK");
        assert_eq!(voice.created_time, "2026-10-31 19:39:49");
    }

    #[tokio::test]
    async fn unused_request_is_not_recorded() {
        // 保证 mock 服务按请求逐个记录（此测试同时验证测试基础设施本身）
        let server =
            MockHttpServer::start(|_request: &RecordedRequest| MockResponse::json("{}")).await;
        assert_eq!(server.request_count(), 0);
    }
}
