//! 百炼音色调试工具的核心逻辑（决策 D19，见 docs/decisions.md）。
//!
//! 只做两件事：列出账号下的自定义音色、删除音色。
//!
//! 边界（刻意保持"哑"）：
//! - 不读写 `voice_cache.json`（那是运行期缓存，属于主程序状态）；
//! - 不提供 enroll（复刻/上传），也不读配置里的自定义音色结构；
//! - 不做任何 prefix 过滤/筛选，只按 voice_id 精确操作；
//! - 凭据与域名由调用方从 `config.toml` 的 `[api]` 取好后传进来。
//!
//! 逻辑放这里（而不是 bin 里）是为了能用 `voice::mock_http` 做单元测试。

use anyhow::{bail, Context, Result};

use crate::voice::api::{self, BailianVoiceApi, RemoteVoice};

/// 取出**确认已完整枚举**的音色列表。
///
/// 分页未完成时直接报错：调试工具的价值是"看到的就是账号里全部音色"，
/// 半截列表容易让人误判（尤其紧接着要执行删除）。
pub async fn list_remote_voices(api: &BailianVoiceApi) -> Result<Vec<RemoteVoice>> {
    let list = api
        .list_voices()
        .await
        .context("获取百炼自定义音色列表失败")?;

    if !list.complete {
        bail!(
            "无法确认音色列表已完整枚举（已取到 {} 个，total={:?}）；请稍后重试",
            list.voices.len(),
            list.total_count
        );
    }

    Ok(list.voices)
}

/// 单条删除的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// 云端已删除。
    Deleted,
    /// 云端本来就不存在（幂等成功：可能是之前删过）。
    AlreadyMissing,
}

/// 删除单个 voice_id。
pub async fn delete_remote_voice(api: &BailianVoiceApi, voice_id: &str) -> Result<DeleteOutcome> {
    let voice_id = voice_id.trim();
    if voice_id.is_empty() {
        bail!("--voice-id 不能为空");
    }

    match api.delete_voice(voice_id).await {
        Ok(()) => Ok(DeleteOutcome::Deleted),
        Err(err) if api::is_resource_missing_error(&err) => Ok(DeleteOutcome::AlreadyMissing),
        Err(err) => Err(err).with_context(|| format!("删除音色 {voice_id} 失败")),
    }
}

/// 批量删除的结果（成功/本来就不存在/失败分别记录，便于终端汇总）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeleteAllReport {
    pub deleted: Vec<String>,
    pub already_missing: Vec<String>,
    /// `(voice_id, 错误详情)`
    pub failed: Vec<(String, String)>,
}

impl DeleteAllReport {
    /// 处理过的音色总数。
    pub fn total(&self) -> usize {
        self.deleted.len() + self.already_missing.len() + self.failed.len()
    }
}

/// 删除给定的全部音色（调用方负责先取完整列表并让用户确认）。
pub async fn delete_all_remote_voices(
    api: &BailianVoiceApi,
    voices: &[RemoteVoice],
) -> DeleteAllReport {
    let mut report = DeleteAllReport::default();

    for voice in voices {
        match delete_remote_voice(api, &voice.voice_id).await {
            Ok(DeleteOutcome::Deleted) => report.deleted.push(voice.voice_id.clone()),
            Ok(DeleteOutcome::AlreadyMissing) => {
                report.already_missing.push(voice.voice_id.clone())
            }
            Err(err) => report
                .failed
                .push((voice.voice_id.clone(), format!("{err:#}"))),
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse, RecordedRequest};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn api(server: &MockHttpServer) -> BailianVoiceApi {
        BailianVoiceApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap()
    }

    fn voice(voice_id: &str) -> RemoteVoice {
        RemoteVoice {
            voice_id: voice_id.to_string(),
            target_model: "qwen-audio-3.1-tts-flash".to_string(),
            status: "OK".to_string(),
            created_time: String::new(),
        }
    }

    fn list_response(voices: &[RemoteVoice]) -> MockResponse {
        let items: Vec<String> = voices
            .iter()
            .map(|voice| {
                format!(
                    r#"{{"voice_id":"{}","target_model":"{}","status":"{}"}}"#,
                    voice.voice_id, voice.target_model, voice.status
                )
            })
            .collect();
        MockResponse::json(format!(
            r#"{{"output":{{"page_index":0,"page_size":50,"total_count":{},"voice_list":[{}]}}}}"#,
            voices.len(),
            items.join(",")
        ))
    }

    fn request_action(request: &RecordedRequest) -> (String, String) {
        let body: serde_json::Value =
            serde_json::from_str(&request.body_text()).unwrap_or_default();
        let action = body["input"]["action"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let voice_id = body["input"]["voice_id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (action, voice_id)
    }

    /// 固定列表 + 固定删除行为（在"全部成功/全部失败"类测试里用）。
    #[derive(Clone, Copy)]
    enum DeleteBehavior {
        Ok,
        Missing,
        ServerError,
    }

    fn fixed_handler(
        voices: Vec<RemoteVoice>,
        behavior: DeleteBehavior,
        deleted_count: Arc<AtomicUsize>,
    ) -> impl Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static {
        move |request: &RecordedRequest| {
            let (action, _voice_id) = request_action(request);
            match action.as_str() {
                "list_voice" => list_response(&voices),
                "delete_voice" => {
                    deleted_count.fetch_add(1, Ordering::SeqCst);
                    match behavior {
                        DeleteBehavior::Ok => MockResponse::json(r#"{"output":{}}"#),
                        DeleteBehavior::Missing => MockResponse::json(
                            r#"{"code":"BadRequest.ResourceNotExist","message":"voice does not exist"}"#,
                        )
                        .status(400),
                        DeleteBehavior::ServerError => {
                            MockResponse::json(r#"{"code":"InternalError","message":"boom"}"#)
                                .status(500)
                        }
                    }
                }
                _ => MockResponse::json(r#"{"code":"Unknown","message":"unexpected action"}"#)
                    .status(400),
            }
        }
    }

    #[tokio::test]
    async fn list_returns_voices() {
        let server = MockHttpServer::start(fixed_handler(
            vec![voice("voice-a"), voice("voice-b")],
            DeleteBehavior::Ok,
            Arc::new(AtomicUsize::new(0)),
        ))
        .await;

        let voices = list_remote_voices(&api(&server)).await.unwrap();
        assert_eq!(voices.len(), 2);
        assert_eq!(voices[0].voice_id, "voice-a");
    }

    #[tokio::test]
    async fn list_rejects_incomplete_enumeration() {
        // 缺少 voice_list 的异常 200 → api 层判定不完整 → admin 层必须拒绝
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;

        let err = list_remote_voices(&api(&server)).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("无法确认音色列表已完整枚举"),
            "错误信息应说明列表不完整: {err:#}"
        );
    }

    #[tokio::test]
    async fn delete_reports_deleted_or_already_missing() {
        let deleted = Arc::new(AtomicUsize::new(0));
        let server =
            MockHttpServer::start(fixed_handler(vec![], DeleteBehavior::Ok, deleted.clone())).await;
        assert_eq!(
            delete_remote_voice(&api(&server), "voice-a").await.unwrap(),
            DeleteOutcome::Deleted
        );
        assert_eq!(deleted.load(Ordering::SeqCst), 1);

        let server = MockHttpServer::start(fixed_handler(
            vec![],
            DeleteBehavior::Missing,
            Arc::new(AtomicUsize::new(0)),
        ))
        .await;
        assert_eq!(
            delete_remote_voice(&api(&server), "already-gone")
                .await
                .unwrap(),
            DeleteOutcome::AlreadyMissing
        );
    }

    #[tokio::test]
    async fn delete_surfaces_api_errors() {
        let server = MockHttpServer::start(fixed_handler(
            vec![],
            DeleteBehavior::ServerError,
            Arc::new(AtomicUsize::new(0)),
        ))
        .await;

        let err = delete_remote_voice(&api(&server), "voice-a")
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("删除音色 voice-a 失败"),
            "错误信息应带上 voice_id: {err:#}"
        );
    }

    #[tokio::test]
    async fn delete_rejects_empty_voice_id_without_network() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;
        let err = delete_remote_voice(&api(&server), "   ").await.unwrap_err();
        assert!(format!("{err:#}").contains("不能为空"));
        assert_eq!(server.request_count(), 0, "空 voice_id 不应发请求");
    }

    #[tokio::test]
    async fn delete_all_reports_each_outcome() {
        // 三个音色：一个成功、一个已不存在、一个服务端错误
        let voices = vec![voice("ok-1"), voice("gone-2"), voice("boom-3")];
        let handler = {
            let voices = voices.clone();
            move |request: &RecordedRequest| -> MockResponse {
                let (action, voice_id) = request_action(request);
                match action.as_str() {
                    "list_voice" => list_response(&voices),
                    "delete_voice" => match voice_id.as_str() {
                        "gone-2" => MockResponse::json(
                            r#"{"code":"BadRequest.ResourceNotExist","message":"voice does not exist"}"#,
                        )
                        .status(400),
                        "boom-3" => {
                            MockResponse::json(r#"{"code":"InternalError","message":"boom"}"#)
                                .status(500)
                        }
                        _ => MockResponse::json(r#"{"output":{}}"#),
                    },
                    _ => MockResponse::json(r#"{"code":"Unknown","message":"unexpected"}"#)
                        .status(400),
                }
            }
        };
        let server = MockHttpServer::start(handler).await;

        let report = delete_all_remote_voices(&api(&server), &voices).await;

        assert_eq!(report.deleted, vec!["ok-1".to_string()]);
        assert_eq!(report.already_missing, vec!["gone-2".to_string()]);
        assert_eq!(report.failed.len(), 1);
        assert_eq!(report.failed[0].0, "boom-3");
        assert_eq!(report.total(), 3);
    }

    #[tokio::test]
    async fn delete_all_with_empty_list_does_nothing() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;

        let report = delete_all_remote_voices(&api(&server), &[]).await;
        assert_eq!(report.total(), 0);
        assert_eq!(server.request_count(), 0);
    }
}
