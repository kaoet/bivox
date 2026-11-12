//! 百炼预编译热词列表的调试 CLI 逻辑（D19/D31，见 docs/decisions.md）。
//!
//! 与 `voice::admin` 保持同样的边界：
//! - 不读写 `cache.json`（那是主程序运行期缓存）；
//! - 不做 prefix 过滤、不提供创建/更新（创建与更新由主程序会话启动时自动完成）；
//! - 只按 `vocabulary_id` 精确列出/删除，逻辑放在这里以便用 mock HTTP 单测。

use anyhow::{Context, Result, bail};

use crate::asr::api::{BailianAsrApi, RemoteVocabulary};
use crate::http::is_resource_missing_error;

/// 取出**确认已完整枚举**的热词列表。
///
/// 官方列表响应没有 `total` 字段，只能靠「短页即末页」判断完整性；
/// 分页异常时直接报错，避免半截列表被误认为全部。
pub async fn list_remote_vocabularies(api: &BailianAsrApi) -> Result<Vec<RemoteVocabulary>> {
    let list = api
        .list_vocabularies()
        .await
        .context("获取百炼预编译热词列表失败")?;

    if !list.complete {
        bail!(
            "无法确认热词列表已完整枚举（已取到 {} 个）；请稍后重试",
            list.vocabularies.len()
        );
    }

    Ok(list.vocabularies)
}

/// 单条删除的结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// 云端已删除。
    Deleted,
    /// 云端本来就不存在（幂等成功）。
    AlreadyMissing,
}

/// 删除单个 `vocabulary_id`。
pub async fn delete_remote_vocabulary(
    api: &BailianAsrApi,
    vocabulary_id: &str,
) -> Result<DeleteOutcome> {
    let vocabulary_id = vocabulary_id.trim();
    if vocabulary_id.is_empty() {
        bail!("--vocabulary-id 不能为空");
    }

    match api.delete_vocabulary(vocabulary_id).await {
        Ok(()) => Ok(DeleteOutcome::Deleted),
        Err(err) if is_resource_missing_error(&err) => Ok(DeleteOutcome::AlreadyMissing),
        Err(err) => Err(err).with_context(|| format!("删除热词列表 {vocabulary_id} 失败")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse, RecordedRequest};
    use serde_json::Value;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn api(server: &MockHttpServer) -> BailianAsrApi {
        BailianAsrApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap()
    }

    fn action(request: &RecordedRequest) -> String {
        let body: Value = serde_json::from_str(&request.body_text()).unwrap_or_default();
        body["input"]["action"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn vocabulary(vocabulary_id: &str) -> RemoteVocabulary {
        RemoteVocabulary {
            vocabulary_id: vocabulary_id.to_string(),
            status: "OK".to_string(),
            created_time: "2026-03-02 18:07:38".to_string(),
            updated_time: "2026-03-02 18:07:38".to_string(),
        }
    }

    fn list_response(vocabularies: &[RemoteVocabulary]) -> MockResponse {
        let items: Vec<String> = vocabularies
            .iter()
            .map(|vocabulary| {
                format!(
                    r#"{{"vocabulary_id":"{}","status":"{}","gmt_modified":"{}"}}"#,
                    vocabulary.vocabulary_id, vocabulary.status, vocabulary.updated_time
                )
            })
            .collect();
        MockResponse::json(format!(
            r#"{{"output":{{"vocabulary_list":[{}]}}}}"#,
            items.join(",")
        ))
    }

    /// 固定列表 + 固定删除行为。
    #[derive(Clone, Copy)]
    enum DeleteBehavior {
        Ok,
        Missing,
        ServerError,
    }

    fn fixed_handler(
        vocabularies: Vec<RemoteVocabulary>,
        behavior: DeleteBehavior,
        deleted_count: Arc<AtomicUsize>,
    ) -> impl Fn(&RecordedRequest) -> MockResponse + Send + Sync + 'static {
        move |request: &RecordedRequest| match action(request).as_str() {
            "list_vocabulary" => list_response(&vocabularies),
            "delete_vocabulary" => {
                deleted_count.fetch_add(1, Ordering::SeqCst);
                match behavior {
                    DeleteBehavior::Ok => MockResponse::json(r#"{"output":{}}"#),
                    DeleteBehavior::Missing => MockResponse::json(
                        r#"{"code":"BadRequest.ResourceNotExist","message":"resource not exist"}"#,
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

    #[tokio::test]
    async fn list_returns_vocabularies() {
        let server = MockHttpServer::start(fixed_handler(
            vec![vocabulary("vocab-a"), vocabulary("vocab-b")],
            DeleteBehavior::Ok,
            Arc::new(AtomicUsize::new(0)),
        ))
        .await;

        let vocabularies = list_remote_vocabularies(&api(&server)).await.unwrap();
        assert_eq!(vocabularies.len(), 2);
        assert_eq!(vocabularies[0].vocabulary_id, "vocab-a");
        assert_eq!(vocabularies[0].updated_time, "2026-03-02 18:07:38");
    }

    #[tokio::test]
    async fn list_rejects_incomplete_enumeration() {
        // 缺少 vocabulary_list 的异常 200 → api 层判定不完整 → admin 层必须拒绝
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;

        let err = list_remote_vocabularies(&api(&server)).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("无法确认热词列表已完整枚举"),
            "错误信息应说明列表不完整: {err:#}"
        );
    }

    #[tokio::test]
    async fn delete_reports_deleted_or_already_missing() {
        let deleted = Arc::new(AtomicUsize::new(0));
        let server =
            MockHttpServer::start(fixed_handler(vec![], DeleteBehavior::Ok, deleted.clone())).await;
        assert_eq!(
            delete_remote_vocabulary(&api(&server), "vocab-a")
                .await
                .unwrap(),
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
            delete_remote_vocabulary(&api(&server), "already-gone")
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

        let err = delete_remote_vocabulary(&api(&server), "vocab-a")
            .await
            .unwrap_err();
        assert!(
            format!("{err:#}").contains("删除热词列表 vocab-a 失败"),
            "错误信息应带上 vocabulary_id: {err:#}"
        );
    }

    #[tokio::test]
    async fn delete_rejects_empty_id_without_network() {
        let server = MockHttpServer::start(|_request| MockResponse::json(r#"{"output":{}}"#)).await;
        let err = delete_remote_vocabulary(&api(&server), "   ")
            .await
            .unwrap_err();
        assert!(format!("{err:#}").contains("不能为空"));
        assert_eq!(server.request_count(), 0, "空 id 不应发请求");
    }
}
