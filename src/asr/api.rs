//! 百炼 ASR 预编译热词管理 API 客户端（D31，见 docs/decisions.md）。
//!
//! 不依赖 SDK，直接调 DashScope 的 HTTP 接口（基址 `https://{host}/api/v1`）：
//!
//! `POST /services/audio/asr/customization`，`model` 固定 `speech-biasing`，
//! 通过请求体 `input.action` 区分动作：
//!
//! - `create_vocabulary`：创建热词列表，返回 `vocabulary_id`；
//! - `query_vocabulary`：查询列表状态（`OK` = 可用于识别）；
//! - `update_vocabulary`：全量替换列表内的词条；
//! - `delete_vocabulary`：删除列表；
//! - `list_vocabulary`：分页列出账号下的列表（返回 `output.vocabulary_list`，
//!   官方响应**没有总数**，只能靠「短页即末页」判断是否取全）。
//!
//! 词条格式为 `{"text": "...", "weight": 50}`：权重固定 50（百炼「超级热词」档位，
//! 官方普通权重为 1–5；超级热词每个词表最多 50 个，D31），
//! 不传 `lang`——识别时同时带 `language_hints` 会导致只有语种匹配的词条生效，
//! 本项目是多语种场景，省略 `lang` 让词条对所有语言生效。

use anyhow::{bail, Context, Result};
use reqwest::{Client, RequestBuilder};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::http::{parse_response, payload};

/// 热词管理服务使用的固定模型名（不是 ASR 模型）。
pub const SPEECH_BIASING_MODEL: &str = "speech-biasing";

/// 创建的预编译热词列表前缀（官方规则：小写字母/数字，≤10 字符）。
pub const VOCABULARY_PREFIX: &str = "kaols";

/// 热词权重：百炼「超级热词」权重 50（普通热词范围 1–5、常用 4）。
///
/// 本项目实测 50 的召回效果更好，固定 50、不提供配置项（D31）；
/// 官方限制每个词表最多 50 个超级热词，因此 [`crate::config::MAX_ASR_HOTWORDS`] = 50。
pub const HOTWORD_WEIGHT: u32 = 50;

/// `list_vocabulary` 每页条数。
const LIST_PAGE_SIZE: usize = 50;
/// 列表接口最多翻页数（防御异常响应导致死循环）。
const LIST_MAX_PAGES: usize = 20;
/// 轮询列表部署状态的间隔。
const READY_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// 百炼账号下的一个预编译热词列表（`list_vocabulary` 的条目）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteVocabulary {
    pub vocabulary_id: String,
    pub status: String,
    pub created_time: String,
    pub updated_time: String,
}

/// `list_vocabulary` 的完整结果（含是否确认枚举完整）。
#[derive(Debug, Clone, Default)]
pub struct RemoteVocabularyList {
    pub vocabularies: Vec<RemoteVocabulary>,
    /// 是否确认已枚举全部列表。
    ///
    /// 官方响应没有总数，只能靠「返回条数 < page_size 即末页」判断；
    /// 分页异常时保守判为不完整，避免 admin 误以为看到的是全部列表。
    pub complete: bool,
}

/// `query_vocabulary` 返回的列表详情。
#[derive(Debug, Clone)]
pub struct VocabularyDetail {
    pub status: String,
    /// 列表绑定的识别模型（部分响应可能缺失）。
    pub target_model: Option<String>,
    /// 原始 output，便于在失败时打印服务端信息。
    pub raw: Value,
}

/// 百炼 ASR 预编译热词 API 客户端。
#[derive(Debug, Clone)]
pub struct BailianAsrApi {
    client: Client,
    api_key: String,
    base_url: String,
}

impl BailianAsrApi {
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

    /// 向热词管理服务发一个带 `input.action` 的请求，返回 `output` 对象。
    async fn customization(&self, action: &str, input: Value) -> Result<Value> {
        let body = json!({
            "model": SPEECH_BIASING_MODEL,
            "input": input,
        });
        let request = self.authorized(
            self.client
                .post(self.url("/services/audio/asr/customization"))
                .json(&body),
        );
        let response = request
            .send()
            .await
            .with_context(|| format!("请求百炼热词管理服务失败（{action}）"))?;
        let value = parse_response(response, action).await?;
        Ok(payload(&value).clone())
    }

    /// 热词条目数组（权重固定 50、不传 `lang`）。
    fn vocabulary_entries(words: &[String]) -> Value {
        Value::Array(
            words
                .iter()
                .map(|word| json!({ "text": word, "weight": HOTWORD_WEIGHT }))
                .collect(),
        )
    }

    /// 创建预编译热词列表，返回 `vocabulary_id`（此时列表可能仍在部署中）。
    pub async fn create_vocabulary(&self, target_model: &str, words: &[String]) -> Result<String> {
        let output = self
            .customization(
                "创建预编译热词列表",
                json!({
                    "action": "create_vocabulary",
                    "target_model": target_model,
                    "prefix": VOCABULARY_PREFIX,
                    "vocabulary": Self::vocabulary_entries(words),
                }),
            )
            .await
            .with_context(|| format!("创建预编译热词列表失败（模型 {target_model}）"))?;

        output
            .get("vocabulary_id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_string)
            .context("百炼创建热词列表响应缺少 vocabulary_id")
    }

    /// 查询单个列表的状态与详情。
    pub async fn query_vocabulary(&self, vocabulary_id: &str) -> Result<VocabularyDetail> {
        let output = self
            .customization(
                "查询预编译热词列表",
                json!({ "action": "query_vocabulary", "vocabulary_id": vocabulary_id }),
            )
            .await
            .with_context(|| format!("查询热词列表 {vocabulary_id} 失败"))?;
        Ok(VocabularyDetail {
            status: output
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            target_model: output
                .get("target_model")
                .and_then(Value::as_str)
                .map(str::to_string),
            raw: output,
        })
    }

    /// 轮询直到列表状态为 `OK`；超时返回 `false`（调用方决定降级策略）。
    pub async fn wait_until_ready(&self, vocabulary_id: &str, timeout: Duration) -> Result<bool> {
        self.wait_until_ready_with(vocabulary_id, timeout, READY_POLL_INTERVAL)
            .await
    }

    /// 可指定轮询间隔的版本（测试用）。
    pub async fn wait_until_ready_with(
        &self,
        vocabulary_id: &str,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<bool> {
        let started = Instant::now();
        loop {
            let detail = self.query_vocabulary(vocabulary_id).await?;
            if is_vocabulary_ready(&detail.status) {
                return Ok(true);
            }
            if started.elapsed() >= timeout {
                log::warn!(
                    "⚠️ 热词列表 {vocabulary_id} 在 {}s 内仍未就绪（最后状态: {:?}）；本次识别暂不使用热词",
                    timeout.as_secs(),
                    detail.status
                );
                return Ok(false);
            }
            tokio::time::sleep(poll_interval).await;
        }
    }

    /// 全量替换列表内的词条（官方建议两次更新间隔 ≥5 分钟）。
    pub async fn update_vocabulary(&self, vocabulary_id: &str, words: &[String]) -> Result<()> {
        self.customization(
            "更新预编译热词列表",
            json!({
                "action": "update_vocabulary",
                "vocabulary_id": vocabulary_id,
                "vocabulary": Self::vocabulary_entries(words),
            }),
        )
        .await
        .with_context(|| format!("更新热词列表 {vocabulary_id} 失败"))?;
        Ok(())
    }

    /// 删除一个预编译热词列表。
    pub async fn delete_vocabulary(&self, vocabulary_id: &str) -> Result<()> {
        self.customization(
            "删除预编译热词列表",
            json!({ "action": "delete_vocabulary", "vocabulary_id": vocabulary_id }),
        )
        .await
        .with_context(|| format!("删除热词列表 {vocabulary_id} 失败"))?;
        Ok(())
    }

    /// 列出账号下全部预编译热词列表（自动翻页）。
    pub async fn list_vocabularies(&self) -> Result<RemoteVocabularyList> {
        let mut vocabularies: Vec<RemoteVocabulary> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut complete = false;

        for page_index in 0..LIST_MAX_PAGES {
            let output = self
                .customization(
                    "列出预编译热词列表",
                    json!({
                        "action": "list_vocabulary",
                        "page_index": page_index,
                        "page_size": LIST_PAGE_SIZE,
                    }),
                )
                .await
                .with_context(|| format!("列出预编译热词列表失败（第 {page_index} 页）"))?;

            let Some(page) = output.get("vocabulary_list").and_then(Value::as_array) else {
                // 缺少 vocabulary_list 的异常 200：不能判定为完整
                break;
            };

            if page.is_empty() {
                complete = true;
                break;
            }

            let mut new_items = 0usize;
            for item in page {
                let vocabulary = parse_remote_vocabulary(item);
                let key = if vocabulary.vocabulary_id.is_empty() {
                    format!("__empty_{}", vocabularies.len())
                } else {
                    vocabulary.vocabulary_id.clone()
                };
                if seen.insert(key) {
                    vocabularies.push(vocabulary);
                    new_items += 1;
                }
            }
            if new_items == 0 {
                // 整页都是重复项：无法继续推进，判为不完整
                break;
            }

            if page.len() < LIST_PAGE_SIZE {
                complete = true;
                break;
            }
        }

        Ok(RemoteVocabularyList {
            vocabularies,
            complete,
        })
    }
}

/// 列表状态是否表示「已就绪、可用于识别」。
pub fn is_vocabulary_ready(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_uppercase().as_str(),
        "OK" | "READY" | "SUCCESS" | "COMPLETED"
    )
}

/// 解析 `list_vocabulary` 返回的一条列表记录。
fn parse_remote_vocabulary(item: &Value) -> RemoteVocabulary {
    let text = |keys: &[&str]| -> String {
        keys.iter()
            .find_map(|key| item.get(*key).and_then(Value::as_str))
            .unwrap_or_default()
            .to_string()
    };

    RemoteVocabulary {
        vocabulary_id: text(&["vocabulary_id", "id"]),
        status: text(&["status"]),
        created_time: text(&["gmt_create", "created_time"]),
        updated_time: text(&["gmt_modified", "updated_time"]),
    }
}

/// 校验词条数量等前置条件，避免明显非法的请求打到服务端。
pub fn validate_vocabulary_words(words: &[String]) -> Result<()> {
    if words.is_empty() {
        bail!("热词列表不能为空");
    }
    crate::config::validate_hotwords(words)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse};
    use serde_json::Value;

    fn test_api(server: &MockHttpServer) -> BailianAsrApi {
        BailianAsrApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap()
    }

    #[tokio::test]
    async fn create_vocabulary_posts_speech_biasing_body() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"output":{"vocabulary_id":"vocab-kaols-1"},"usage":{"count":1}}"#,
            )
        })
        .await;
        let api = test_api(&server);

        let id = api
            .create_vocabulary(
                "qwen-audio-3.1-asr-flash-streaming",
                &["原神".to_string(), "Kaoet".to_string()],
            )
            .await
            .unwrap();
        assert_eq!(id, "vocab-kaols-1");

        let requests = server.requests();
        assert_eq!(requests[0].path, "/api/v1/services/audio/asr/customization");
        assert_eq!(requests[0].header("authorization"), Some("Bearer sk-test"));
        let body: Value = serde_json::from_str(&requests[0].body_text()).unwrap();
        assert_eq!(body["model"], "speech-biasing");
        assert_eq!(body["input"]["action"], "create_vocabulary");
        assert_eq!(
            body["input"]["target_model"],
            "qwen-audio-3.1-asr-flash-streaming"
        );
        assert_eq!(body["input"]["prefix"], "kaols");
        assert_eq!(body["input"]["vocabulary"][0]["text"], "原神");
        assert_eq!(
            body["input"]["vocabulary"][0]["weight"], HOTWORD_WEIGHT,
            "热词权重固定为超级热词 50（D31）"
        );
        assert!(
            body["input"]["vocabulary"][0].get("lang").is_none(),
            "不应限定词条语种（否则会被 language_hints 过滤）"
        );
    }

    #[tokio::test]
    async fn missing_vocabulary_id_in_create_response_is_reported() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;
        let api = test_api(&server);
        let err = api
            .create_vocabulary("m", &["词".to_string()])
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("缺少 vocabulary_id"), "{err:#}");
    }

    #[tokio::test]
    async fn query_vocabulary_parses_status_and_target_model() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"output":{"status":"OK","target_model":"qwen-audio-3.1-asr-flash-streaming","vocabulary":[{"text":"原神","weight":50}]}}"#,
            )
        })
        .await;
        let api = test_api(&server);

        let detail = api.query_vocabulary("vocab-1").await.unwrap();
        assert_eq!(detail.status, "OK");
        assert_eq!(
            detail.target_model.as_deref(),
            Some("qwen-audio-3.1-asr-flash-streaming")
        );
        let body: Value = serde_json::from_str(&server.requests()[0].body_text()).unwrap();
        assert_eq!(body["input"]["action"], "query_vocabulary");
        assert_eq!(body["input"]["vocabulary_id"], "vocab-1");
    }

    #[tokio::test]
    async fn update_vocabulary_replaces_all_entries() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;
        let api = test_api(&server);

        api.update_vocabulary("vocab-1", &["新词".to_string()])
            .await
            .unwrap();

        let body: Value = serde_json::from_str(&server.requests()[0].body_text()).unwrap();
        assert_eq!(body["input"]["action"], "update_vocabulary");
        assert_eq!(body["input"]["vocabulary_id"], "vocab-1");
        assert_eq!(body["input"]["vocabulary"][0]["text"], "新词");
        assert_eq!(body["input"]["vocabulary"][0]["weight"], HOTWORD_WEIGHT);
    }

    #[tokio::test]
    async fn delete_vocabulary_posts_delete_action() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;
        let api = test_api(&server);

        api.delete_vocabulary("vocab-1").await.unwrap();

        let body: Value = serde_json::from_str(&server.requests()[0].body_text()).unwrap();
        assert_eq!(body["input"]["action"], "delete_vocabulary");
        assert_eq!(body["input"]["vocabulary_id"], "vocab-1");
    }

    #[tokio::test]
    async fn wait_until_ready_polls_until_ok() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let calls = Arc::new(AtomicUsize::new(0));
        let calls_server = calls.clone();
        let server = MockHttpServer::start(move |_request| {
            match calls_server.fetch_add(1, Ordering::SeqCst) {
                0 => MockResponse::json(r#"{"output":{"status":"UNDEPLOYED"}}"#),
                1 => MockResponse::json(r#"{"output":{"status":"UNDEPLOYED"}}"#),
                _ => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            }
        })
        .await;
        let api = test_api(&server);

        let ready = api
            .wait_until_ready_with("vocab-1", Duration::from_secs(2), Duration::from_millis(5))
            .await
            .unwrap();
        assert!(ready);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn wait_until_ready_returns_false_on_timeout() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"output":{"status":"UNDEPLOYED"}}"#)
        })
        .await;
        let api = test_api(&server);

        let ready = api
            .wait_until_ready_with(
                "vocab-1",
                Duration::from_millis(20),
                Duration::from_millis(5),
            )
            .await
            .unwrap();
        assert!(!ready, "超时后应返回 false 由调用方降级");
    }

    #[tokio::test]
    async fn list_vocabularies_paginates_until_short_page() {
        let server = MockHttpServer::start(|request| {
            let body: Value = serde_json::from_str(&request.body_text()).unwrap();
            let page_index = body["input"]["page_index"].as_u64().unwrap();
            if page_index == 0 {
                let items: Vec<String> = (0..50)
                    .map(|i| format!(r#"{{"vocabulary_id":"v{i}","status":"OK"}}"#))
                    .collect();
                MockResponse::json(format!(
                    r#"{{"output":{{"vocabulary_list":[{}]}}}}"#,
                    items.join(",")
                ))
            } else {
                MockResponse::json(
                    r#"{"output":{"vocabulary_list":[{"vocabulary_id":"last","status":"OK","gmt_modified":"2026-03-02 18:07:38"}]}}"#,
                )
            }
        })
        .await;
        let api = test_api(&server);

        let list = api.list_vocabularies().await.unwrap();
        assert_eq!(list.vocabularies.len(), 51);
        assert_eq!(list.vocabularies[50].vocabulary_id, "last");
        assert_eq!(list.vocabularies[50].updated_time, "2026-03-02 18:07:38");
        assert!(list.complete, "短页应判定为枚举完整");
        assert_eq!(server.request_count(), 2);
    }

    #[tokio::test]
    async fn list_vocabularies_without_field_is_incomplete() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;
        let api = test_api(&server);

        let list = api.list_vocabularies().await.unwrap();
        assert!(list.vocabularies.is_empty());
        assert!(!list.complete, "缺少 vocabulary_list 时不能判定为完整");
    }

    #[tokio::test]
    async fn list_vocabularies_repeated_full_pages_are_incomplete() {
        // 每页都是满页（50 条）且内容相同：去重后没有新增，无法继续推进 → 不完整。
        let server = MockHttpServer::start(|_request| {
            let items: Vec<String> = (0..50)
                .map(|i| format!(r#"{{"vocabulary_id":"v{i}"}}"#))
                .collect();
            MockResponse::json(format!(
                r#"{{"output":{{"vocabulary_list":[{}]}}}}"#,
                items.join(",")
            ))
        })
        .await;
        let api = test_api(&server);

        let list = api.list_vocabularies().await.unwrap();
        assert_eq!(list.vocabularies.len(), 50, "重复项应被去重");
        assert!(!list.complete, "重复页无法取全时不能判定为完整");
        assert_eq!(server.request_count(), 2, "发现无新增项后应立即停止翻页");
    }

    #[tokio::test]
    async fn api_error_is_reported_with_action_and_code() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(
                r#"{"code":"InvalidParameter","message":"prefix should be english letter and number"}"#,
            )
            .status(400)
        })
        .await;
        let api = test_api(&server);

        let err = api
            .create_vocabulary("m", &["词".to_string()])
            .await
            .unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("HTTP 400"), "{text}");
        assert!(text.contains("InvalidParameter"), "{text}");
    }

    #[test]
    fn validates_words_before_sending() {
        assert!(
            validate_vocabulary_words(&[]).is_err(),
            "空热词列表不应发请求"
        );
        assert!(validate_vocabulary_words(&["原神".to_string()]).is_ok());
        assert!(
            validate_vocabulary_words(&["一二三四五六七八九十一二三四五六".to_string()]).is_err()
        );
    }
}
