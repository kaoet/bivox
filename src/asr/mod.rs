//! ASR 预编译热词（D31，见 docs/decisions.md）。
//!
//! 配置里的 `[asr] hotwords` 在**会话启动时**自动同步到百炼的预编译热词列表：
//!
//! - 第一次使用某个 ASR 模型的热词：`create_vocabulary` 创建列表，拿到的
//!   `vocabulary_id` 立即写入 `cache.json`，再轮询到 `status == "OK"`；
//! - 之后启动：`query_vocabulary` 校验缓存里的列表仍可用，词表没变就直接复用；
//! - 词表变了：`update_vocabulary` 全量替换（官方建议两次更新间隔 ≥5 分钟，
//!   客户端按缓存里的 `updated_at` 限速，避免「更新成功但实际没生效」）；
//! - 云端列表被删除：自动重新创建并覆盖缓存；
//! - 任何一步失败都**不阻断实时会话**：本次识别不带 `vocabulary_id` 继续跑，
//!   通过 `PipelineEvent::Warning` 在界面提示（D8/D31）。
//!
//! GUI 不访问网络、不读写缓存；本模块只在 `pipeline::session` 启动时调用。

pub mod admin;
pub mod api;

use anyhow::{Context, Result};
use std::time::Duration;

use crate::cache::{AppCache, HotwordCacheEntry};
use crate::config::AppConfig;

pub use api::BailianAsrApi;

/// 创建/更新后等待列表就绪的最长时间。
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// 官方建议两次热词列表更新的最小间隔（秒）。
const MIN_UPDATE_INTERVAL_SECS: u64 = 300;

/// 会话启动时解析预编译热词的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VocabularyResolution {
    /// 本次识别应传给 run-task 的 `vocabulary_id`；`None` = 不带热词继续识别。
    pub vocabulary_id: Option<String>,
    /// 需要展示给用户的告警（失败降级、更新被限速等）；`None` = 一切正常。
    pub warning: Option<String>,
}

impl VocabularyResolution {
    fn ready(vocabulary_id: String) -> Self {
        Self {
            vocabulary_id: Some(vocabulary_id),
            warning: None,
        }
    }

    fn skipped(warning: impl Into<String>) -> Self {
        Self {
            vocabulary_id: None,
            warning: Some(warning.into()),
        }
    }
}

/// 根据配置构造百炼 ASR 热词管理 API 客户端。
pub fn api_from_config(config: &AppConfig) -> Result<BailianAsrApi> {
    let api_key = config
        .api
        .key
        .clone()
        .context("未配置 API Key，请先在「设置」页填写")?;
    BailianAsrApi::new(api_key, config.http_api_base())
}

/// 会话启动时确保配置里的热词已在百炼侧注册，返回本次应使用的 `vocabulary_id`。
///
/// 这是运行期入口：GUI 保存配置后由下一次会话启动触发；失败只降级不返回错误。
pub async fn resolve_vocabulary(config: &AppConfig) -> VocabularyResolution {
    let desired = crate::config::normalize_hotwords(&config.asr.hotwords);
    if desired.is_empty() {
        return VocabularyResolution::default();
    }

    let api = match api_from_config(config) {
        Ok(api) => api,
        Err(err) => return VocabularyResolution::skipped(format!("{err:#}")),
    };
    let mut cache = match AppCache::load(config.cache_path()) {
        Ok(cache) => cache,
        Err(err) => return VocabularyResolution::skipped(format!("{err:#}")),
    };

    match ensure_vocabulary_with(
        &api,
        &mut cache,
        &config.asr.model,
        &desired,
        crate::cache::unix_now(),
    )
    .await
    {
        Ok(resolution) => resolution,
        Err(err) => {
            // 失败时若缓存里仍有可用 id，先沿用旧词表（可能不是最新配置，但有胜于无）。
            let fallback = cache
                .hotword_entry(&config.asr.model)
                .map(|entry| entry.vocabulary_id.clone());
            let warning = format!("预编译热词不可用，本次识别不带最新热词：{err:#}");
            log::warn!("⚠️ {warning}");
            VocabularyResolution {
                vocabulary_id: fallback,
                warning: Some(warning),
            }
        }
    }
}

/// [`resolve_vocabulary`] 的可测试核心：依赖注入 API 客户端、缓存与当前时间。
///
/// `now` 由调用方传入（Unix 秒），用于测试 5 分钟更新限速。
pub async fn ensure_vocabulary_with(
    api: &BailianAsrApi,
    cache: &mut AppCache,
    target_model: &str,
    desired_hotwords: &[String],
    now: u64,
) -> Result<VocabularyResolution> {
    let desired = crate::config::normalize_hotwords(desired_hotwords);
    if desired.is_empty() {
        return Ok(VocabularyResolution::default());
    }

    let Some(existing) = cache.hotword_entry(target_model).cloned() else {
        return create_vocabulary(api, cache, target_model, &desired, now).await;
    };

    // 1) 校验缓存里的列表在云端是否仍存在、是否就绪。
    match api.query_vocabulary(&existing.vocabulary_id).await {
        Ok(detail) => {
            if detail
                .target_model
                .as_deref()
                .is_some_and(|model| model != target_model)
            {
                log::warn!(
                    "⚠️ 缓存的热词列表 {} 属于模型 {}，与当前 ASR 模型 {} 不一致，将重新创建",
                    existing.vocabulary_id,
                    detail.target_model.as_deref().unwrap_or("?"),
                    target_model
                );
                return create_vocabulary(api, cache, target_model, &desired, now).await;
            }
            if !api::is_vocabulary_ready(&detail.status) {
                log::info!(
                    "⏳ 热词列表 {} 状态为 {:?}，等待就绪...",
                    existing.vocabulary_id,
                    detail.status
                );
                if !api
                    .wait_until_ready(&existing.vocabulary_id, READY_TIMEOUT)
                    .await?
                {
                    return Ok(VocabularyResolution {
                        vocabulary_id: None,
                        warning: Some(format!(
                            "热词列表 {} 尚未就绪，本次识别暂不使用热词；下次启动会继续等待",
                            existing.vocabulary_id
                        )),
                    });
                }
            }
        }
        Err(err) if crate::http::is_resource_missing_error(&err) => {
            log::warn!(
                "♻️ 热词列表 {} 已不存在（可能被删除），将重新创建",
                existing.vocabulary_id
            );
            return create_vocabulary(api, cache, target_model, &desired, now).await;
        }
        Err(err) => {
            // 网络/服务端错误：无法确认远端状态，沿用缓存 id；词表或权重若变了则提示未更新。
            let outdated = existing.hotwords != desired || existing.weight != api::HOTWORD_WEIGHT;
            let warning = outdated.then(|| {
                format!(
                    "无法校验/更新热词列表 {}（{err:#}），本次沿用旧词表继续识别",
                    existing.vocabulary_id
                )
            });
            if let Some(warning) = &warning {
                log::warn!("⚠️ {warning}");
            } else {
                log::warn!(
                    "⚠️ 无法校验热词列表 {} 是否有效（{err:#}），先继续使用缓存",
                    existing.vocabulary_id
                );
            }
            return Ok(VocabularyResolution {
                vocabulary_id: Some(existing.vocabulary_id),
                warning,
            });
        }
    }

    // 2) 词表没变（且权重与当前代码一致）：直接复用。
    if existing.hotwords == desired && existing.weight == api::HOTWORD_WEIGHT {
        log::info!(
            "🔥 预编译热词列表命中缓存: {}（{} 条）",
            existing.vocabulary_id,
            desired.len()
        );
        return Ok(VocabularyResolution::ready(existing.vocabulary_id));
    }

    // 3) 官方建议两次更新间隔 ≥5 分钟：太频繁时先沿用旧词表，下次启动再重试。
    if now.saturating_sub(existing.updated_at) < MIN_UPDATE_INTERVAL_SECS {
        let what = match (
            existing.hotwords != desired,
            existing.weight != api::HOTWORD_WEIGHT,
        ) {
            (true, true) => "热词词表与默认权重",
            (true, false) => "热词词表",
            (false, true) => "热词默认权重",
            (false, false) => "热词列表",
        };
        let warning = format!(
            "{what}有变化，但距上次列表更新不足 5 分钟（官方建议间隔 ≥5 分钟），\
             本次仍使用旧词表；稍后开始会话会自动重试"
        );
        log::warn!("⚠️ {warning}");
        return Ok(VocabularyResolution {
            vocabulary_id: Some(existing.vocabulary_id),
            warning: Some(warning),
        });
    }

    // 4) 全量更新词表，写回缓存后再等待就绪。
    log::info!(
        "🔥 更新预编译热词列表 {}（{} 条）...",
        existing.vocabulary_id,
        desired.len()
    );
    match api
        .update_vocabulary(&existing.vocabulary_id, &desired)
        .await
    {
        Ok(()) => {
            let vocabulary_id = existing.vocabulary_id;
            save_hotword_entry(cache, target_model, &vocabulary_id, &desired, now);
            match api.wait_until_ready(&vocabulary_id, READY_TIMEOUT).await {
                Ok(true) => Ok(VocabularyResolution::ready(vocabulary_id)),
                Ok(false) => Ok(VocabularyResolution {
                    vocabulary_id: None,
                    warning: Some(format!(
                        "热词列表 {vocabulary_id} 更新后尚未就绪，本次识别暂不使用热词；下次启动会继续等待"
                    )),
                }),
                Err(err) => Ok(VocabularyResolution {
                    vocabulary_id: None,
                    warning: Some(format!(
                        "热词列表 {vocabulary_id} 更新后查询状态失败：{err:#}；本次识别暂不使用热词"
                    )),
                }),
            }
        }
        Err(err) if crate::http::is_resource_missing_error(&err) => {
            log::warn!(
                "♻️ 更新热词列表 {} 时发现它已不存在，将重新创建",
                existing.vocabulary_id
            );
            create_vocabulary(api, cache, target_model, &desired, now).await
        }
        Err(err) => {
            let warning = format!(
                "更新热词列表 {} 失败（{err:#}），本次沿用旧词表继续识别",
                existing.vocabulary_id
            );
            log::warn!("⚠️ {warning}");
            Ok(VocabularyResolution {
                vocabulary_id: Some(existing.vocabulary_id),
                warning: Some(warning),
            })
        }
    }
}

/// 创建新列表：拿到 id 后立刻写缓存（满足「第一次创建就保存 vocabulary_id」），再轮询就绪。
async fn create_vocabulary(
    api: &BailianAsrApi,
    cache: &mut AppCache,
    target_model: &str,
    desired: &[String],
    now: u64,
) -> Result<VocabularyResolution> {
    log::info!(
        "📤 创建预编译热词列表（模型 {}，{} 条）...",
        target_model,
        desired.len()
    );
    let vocabulary_id = api.create_vocabulary(target_model, desired).await?;
    log::info!("🔥 预编译热词列表已创建: {vocabulary_id}");
    save_hotword_entry(cache, target_model, &vocabulary_id, desired, now);

    match api.wait_until_ready(&vocabulary_id, READY_TIMEOUT).await {
        Ok(true) => Ok(VocabularyResolution::ready(vocabulary_id)),
        Ok(false) => Ok(VocabularyResolution {
            vocabulary_id: None,
            warning: Some(format!(
                "热词列表 {vocabulary_id} 创建后尚未就绪，本次识别暂不使用热词；下次启动会继续等待"
            )),
        }),
        Err(err) => Ok(VocabularyResolution {
            vocabulary_id: None,
            warning: Some(format!(
                "热词列表 {vocabulary_id} 创建后查询状态失败：{err:#}；本次识别暂不使用热词"
            )),
        }),
    }
}

/// 写入热词缓存并尽力落盘（保存失败不影响本次会话，下次会重新校验/创建）。
fn save_hotword_entry(
    cache: &mut AppCache,
    target_model: &str,
    vocabulary_id: &str,
    hotwords: &[String],
    now: u64,
) {
    cache.set_hotword_entry(
        target_model,
        HotwordCacheEntry {
            vocabulary_id: vocabulary_id.to_string(),
            hotwords: hotwords.to_vec(),
            weight: api::HOTWORD_WEIGHT,
            updated_at: now,
        },
    );
    if let Err(err) = cache.save() {
        log::warn!("⚠️ 保存热词缓存失败（不影响本次识别）：{err:#}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse};
    use serde_json::Value;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const MODEL: &str = "qwen-audio-3.1-asr-flash-streaming";

    fn temp_cache_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "bivox-hotwords-{tag}-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn cache_with_entry(
        tag: &str,
        hotwords: &[&str],
        updated_at: u64,
    ) -> (std::path::PathBuf, AppCache) {
        let path = temp_cache_path(tag);
        let mut cache = AppCache::load(&path).unwrap();
        cache.set_hotword_entry(
            MODEL,
            HotwordCacheEntry {
                vocabulary_id: "vocab-old".to_string(),
                hotwords: hotwords.iter().map(|word| (*word).to_string()).collect(),
                weight: api::HOTWORD_WEIGHT,
                updated_at,
            },
        );
        (path, cache)
    }

    fn words(values: &[&str]) -> Vec<String> {
        values.iter().map(|word| (*word).to_string()).collect()
    }

    fn api(server: &MockHttpServer) -> BailianAsrApi {
        BailianAsrApi::new("sk-test", format!("{}/api/v1", server.base_url())).unwrap()
    }

    fn action(request: &crate::voice::mock_http::RecordedRequest) -> String {
        let body: Value = serde_json::from_str(&request.body_text()).unwrap_or_default();
        body["input"]["action"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test]
    async fn first_start_creates_vocabulary_and_saves_id_before_polling() {
        let create_count = Arc::new(AtomicUsize::new(0));
        let count = create_count.clone();
        let server = MockHttpServer::start(move |request| match action(request).as_str() {
            "create_vocabulary" => {
                count.fetch_add(1, Ordering::SeqCst);
                MockResponse::json(r#"{"output":{"vocabulary_id":"vocab-new"}}"#)
            }
            "query_vocabulary" => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            other => MockResponse::json(format!(
                r#"{{"code":"Unexpected","message":"unexpected action {other}"}}"#
            ))
            .status(500),
        })
        .await;
        let api = api(&server);
        let path = temp_cache_path("create");
        let mut cache = AppCache::load(&path).unwrap();

        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["原神"]), 1000)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-new"));
        assert!(resolution.warning.is_none());
        assert_eq!(create_count.load(Ordering::SeqCst), 1);
        let entry = cache.hotword_entry(MODEL).expect("应写入热词缓存");
        assert_eq!(entry.vocabulary_id, "vocab-new");
        assert_eq!(entry.hotwords, words(&["原神"]));
        assert_eq!(entry.updated_at, 1000);
        assert!(path.exists(), "热词 id 应立即落盘");

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn second_start_reuses_cached_vocabulary_without_update() {
        let server = MockHttpServer::start(|request| match action(request).as_str() {
            "query_vocabulary" => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            other => MockResponse::json(format!(
                r#"{{"code":"Unexpected","message":"unexpected action {other}"}}"#
            ))
            .status(500),
        })
        .await;
        let api = api(&server);
        let (path, mut cache) = cache_with_entry("reuse", &["原神"], 1000);

        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["原神"]), 2000)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-old"));
        assert!(resolution.warning.is_none());
        assert_eq!(server.request_count(), 1, "只应查询一次，不创建/更新");

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn changed_hotwords_update_vocabulary_after_interval() {
        let server = MockHttpServer::start(|request| match action(request).as_str() {
            "query_vocabulary" => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            "update_vocabulary" => {
                let body: Value = serde_json::from_str(&request.body_text()).unwrap();
                assert_eq!(body["input"]["vocabulary"][0]["text"], "新词");
                assert_eq!(
                    body["input"]["vocabulary"][0]["weight"],
                    api::HOTWORD_WEIGHT
                );
                MockResponse::json(r#"{"output":{}}"#)
            }
            other => MockResponse::json(format!(
                r#"{{"code":"Unexpected","message":"unexpected action {other}"}}"#
            ))
            .status(500),
        })
        .await;
        let api = api(&server);
        let (path, mut cache) = cache_with_entry("update", &["旧词"], 1000);

        // 距上次更新 600s > 300s，允许更新
        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["新词"]), 1600)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-old"));
        assert!(resolution.warning.is_none());
        let entry = cache.hotword_entry(MODEL).unwrap();
        assert_eq!(entry.hotwords, words(&["新词"]));
        assert_eq!(entry.updated_at, 1600);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn weight_change_triggers_vocabulary_update() {
        // 代码里的默认权重变化（例如 4 → 50）也要让已缓存的列表更新一次。
        let server = MockHttpServer::start(|request| match action(request).as_str() {
            "query_vocabulary" => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            "update_vocabulary" => {
                let body: Value = serde_json::from_str(&request.body_text()).unwrap();
                assert_eq!(
                    body["input"]["vocabulary"][0]["weight"],
                    api::HOTWORD_WEIGHT
                );
                MockResponse::json(r#"{"output":{}}"#)
            }
            other => MockResponse::json(format!(
                r#"{{"code":"Unexpected","message":"unexpected action {other}"}}"#
            ))
            .status(500),
        })
        .await;
        let api = api(&server);
        let (path, mut cache) = cache_with_entry("weight-change", &["原神"], 1000);
        let mut old = cache.hotword_entry(MODEL).unwrap().clone();
        old.weight = 4;
        cache.set_hotword_entry(MODEL, old);

        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["原神"]), 1600)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-old"));
        assert_eq!(
            cache.hotword_entry(MODEL).unwrap().weight,
            api::HOTWORD_WEIGHT,
            "更新后缓存应记录当前权重"
        );

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn too_frequent_update_is_skipped_with_warning() {
        let server = MockHttpServer::start(|request| match action(request).as_str() {
            "query_vocabulary" => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            other => MockResponse::json(format!(
                r#"{{"code":"Unexpected","message":"unexpected action {other}"}}"#
            ))
            .status(500),
        })
        .await;
        let api = api(&server);
        let (path, mut cache) = cache_with_entry("too-frequent", &["旧词"], 1000);

        // 距上次更新只过了 60s，不应发起 update
        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["新词"]), 1060)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-old"));
        assert!(
            resolution
                .warning
                .as_deref()
                .unwrap_or("")
                .contains("不足 5 分钟"),
            "{resolution:?}"
        );
        assert_eq!(
            cache.hotword_entry(MODEL).unwrap().hotwords,
            words(&["旧词"]),
            "跳过更新时缓存快照不应改变"
        );

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn missing_remote_vocabulary_is_recreated() {
        let server = MockHttpServer::start(|request| {
            let body: Value = serde_json::from_str(&request.body_text()).unwrap_or_default();
            match action(request).as_str() {
                "query_vocabulary" if body["input"]["vocabulary_id"] == "vocab-old" => {
                    MockResponse::json(
                        r#"{"code":"BadRequest.ResourceNotExist","message":"resource not exist"}"#,
                    )
                    .status(400)
                }
                "query_vocabulary" => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
                "create_vocabulary" => {
                    MockResponse::json(r#"{"output":{"vocabulary_id":"vocab-new"}}"#)
                }
                other => MockResponse::json(format!(
                    r#"{{"code":"Unexpected","message":"unexpected action {other}"}}"#
                ))
                .status(500),
            }
        })
        .await;
        let api = api(&server);
        let (path, mut cache) = cache_with_entry("recreate", &["原神"], 1000);

        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["原神"]), 2000)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-new"));
        assert_eq!(
            cache.hotword_entry(MODEL).unwrap().vocabulary_id,
            "vocab-new"
        );

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn update_failure_keeps_old_vocabulary_with_warning() {
        let server = MockHttpServer::start(|request| match action(request).as_str() {
            "query_vocabulary" => MockResponse::json(r#"{"output":{"status":"OK"}}"#),
            "update_vocabulary" => {
                MockResponse::json(r#"{"code":"InternalError","message":"boom"}"#).status(500)
            }
            other => MockResponse::json(format!(
                r#"{{"code":"Unexpected","message":"unexpected action {other}"}}"#
            ))
            .status(500),
        })
        .await;
        let api = api(&server);
        let (path, mut cache) = cache_with_entry("update-fail", &["旧词"], 1000);

        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["新词"]), 1600)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-old"));
        assert!(
            resolution
                .warning
                .as_deref()
                .unwrap_or("")
                .contains("沿用旧词表"),
            "{resolution:?}"
        );
        assert_eq!(
            cache.hotword_entry(MODEL).unwrap().hotwords,
            words(&["旧词"])
        );

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn query_network_error_reuses_cache_without_warning_when_unchanged() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"code":"InternalError","message":"boom"}"#).status(500)
        })
        .await;
        let api = api(&server);
        let (path, mut cache) = cache_with_entry("query-fail", &["原神"], 1000);

        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &words(&["原神"]), 2000)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id.as_deref(), Some("vocab-old"));
        assert!(resolution.warning.is_none(), "词表没变时不必打扰用户");

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn empty_hotwords_do_not_touch_network() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"code":"ShouldNotCall","message":"no request expected"}"#)
                .status(500)
        })
        .await;
        let api = api(&server);
        let mut cache = AppCache::load(temp_cache_path("empty")).unwrap();

        let resolution = ensure_vocabulary_with(&api, &mut cache, MODEL, &[], 1000)
            .await
            .unwrap();

        assert_eq!(resolution.vocabulary_id, None);
        assert!(resolution.warning.is_none());
        assert_eq!(server.request_count(), 0, "没有热词时不应访问百炼");
    }
}
