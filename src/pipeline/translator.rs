//! 翻译 worker：独立 tokio 任务顺序处理翻译请求，结果按发送顺序回传。
//!
//! 会话主循环不能 `await` HTTP：它是 `biased select`，打字输入优先，
//! 阻塞一次翻译会让打字变慢（D5 的同类约束）。因此这里用无界通道把任务送给
//! 后台任务，再把结果送回会话的 `select`；worker 单 in-flight，天然保持 TTS 顺序。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use crate::client::qwen_mt::{QwenMtClient, TranslateRequest};

/// 待翻译的一段文本。
#[derive(Debug, Clone)]
pub struct TranslationJob {
    /// 会话代号：打字打断 ASR 句时递增，旧结果全部丢弃。
    pub generation: u64,
    pub text: String,
    /// 该片段使用的源语言（ISO 代码；判定失败时可用 `auto`）。
    pub source_language: String,
    /// 源语言与目标语言相同时跳过 HTTP，直接回传原文（保证与原句顺序一致）。
    pub skip_translation: bool,
    /// 该片段是否携带本句句终。
    pub is_sentence_end: bool,
}

/// 翻译结果。
#[derive(Debug, Clone)]
pub struct TranslationDone {
    pub generation: u64,
    pub is_sentence_end: bool,
    /// 成功为译文；失败为带阶段的中文错误（`anyhow::Error` 不可 `Clone`）。
    pub result: Result<String, String>,
}

/// 启动翻译 worker。
///
/// `running` 复用会话的运行标志：用户停止后 worker 在下一个任务边界退出。
/// 返回的句柄需要由会话在收尾时等待（与 ASR/TTS 任务同样的 3s 上限）。
pub fn spawn(
    client: QwenMtClient,
    target_language: String,
    running: Arc<AtomicBool>,
) -> (
    UnboundedSender<TranslationJob>,
    UnboundedReceiver<TranslationDone>,
    JoinHandle<()>,
) {
    let (tx_job, mut rx_job) = unbounded_channel::<TranslationJob>();
    let (tx_done, rx_done) = unbounded_channel::<TranslationDone>();
    let handle = tokio::spawn(async move {
        while let Some(job) = rx_job.recv().await {
            if !running.load(Ordering::SeqCst) {
                break;
            }
            let result = if job.skip_translation {
                Ok(job.text.clone())
            } else {
                client
                    .translate(&TranslateRequest::new(
                        &job.text,
                        &job.source_language,
                        &target_language,
                    ))
                    .await
                    // 统一成 String：事件/日志只要可读的错误文本
                    .map_err(|error| format!("{error:#}"))
            };
            let done = TranslationDone {
                generation: job.generation,
                is_sentence_end: job.is_sentence_end,
                result,
            };
            if tx_done.send(done).is_err() {
                // 会话已退出
                break;
            }
        }
    });
    (tx_job, rx_done, handle)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::voice::mock_http::{MockHttpServer, MockResponse};

    fn client_for(server: &MockHttpServer) -> QwenMtClient {
        QwenMtClient::new(
            "sk-test",
            "qwen-mt-flash",
            format!("{}/compatible-mode/v1/chat/completions", server.base_url()),
        )
        .expect("构造翻译客户端")
    }

    fn job(text: &str, is_sentence_end: bool) -> TranslationJob {
        TranslationJob {
            generation: 1,
            text: text.to_string(),
            source_language: "zh".to_string(),
            skip_translation: false,
            is_sentence_end,
        }
    }

    #[tokio::test]
    async fn processes_jobs_in_order_and_passes_generation() {
        let counter = Arc::new(AtomicUsize::new(0));
        let handler_counter = counter.clone();
        let server = MockHttpServer::start(move |_request| {
            let index = handler_counter.fetch_add(1, Ordering::SeqCst) + 1;
            MockResponse::json(format!(
                r#"{{"choices":[{{"message":{{"content":"translated-{index}"}}}}]}}"#
            ))
        })
        .await;

        let running = Arc::new(AtomicBool::new(true));
        let (tx_job, mut rx_done, handle) =
            spawn(client_for(&server), "en".to_string(), running.clone());

        tx_job.send(job("一", false)).expect("发送任务 1");
        tx_job.send(job("二", false)).expect("发送任务 2");
        tx_job.send(job("三", true)).expect("发送任务 3");

        let mut done = Vec::new();
        for _ in 0..3 {
            done.push(rx_done.recv().await.expect("等待翻译结果"));
        }

        assert!(done.iter().all(|item| item.generation == 1));
        assert_eq!(
            done.iter()
                .map(|item| item.result.as_deref().unwrap_or(""))
                .collect::<Vec<_>>(),
            vec!["translated-1", "translated-2", "translated-3"]
        );
        assert!(done[2].is_sentence_end);
        assert_eq!(server.request_count(), 3);

        drop(tx_job);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle).await;
    }

    #[tokio::test]
    async fn skips_http_when_source_equals_target() {
        let server = MockHttpServer::start(|_request| {
            MockResponse::json(r#"{"choices":[{"message":{"content":"should-not-be-used"}}]}"#)
        })
        .await;

        let running = Arc::new(AtomicBool::new(true));
        let (tx_job, mut rx_done, _handle) = spawn(client_for(&server), "zh".to_string(), running);

        let mut skipped = job("你好", true);
        skipped.skip_translation = true;
        tx_job.send(skipped).expect("发送跳过任务");

        let done = rx_done.recv().await.expect("等待结果");
        assert_eq!(done.result.as_deref(), Ok("你好"));
        assert_eq!(server.request_count(), 0);
    }

    #[tokio::test]
    async fn stops_after_running_flag_cleared() {
        let server = MockHttpServer::start(|_request| MockResponse::json("{}")).await;

        let running = Arc::new(AtomicBool::new(false));
        let (tx_job, _rx_done, handle) = spawn(client_for(&server), "en".to_string(), running);

        tx_job.send(job("不会处理", true)).expect("发送任务");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("worker 应在运行标志清除后退出");
        assert_eq!(server.request_count(), 0);
    }
}
