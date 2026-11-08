//! 百炼 HTTP JSON 接口的公共错误与响应解析。
//!
//! 声音复刻（`/files`、`/services/audio/tts/customization`）与 ASR 预编译热词
//! （`/services/audio/asr/customization`）使用同一套响应约定：
//! - 成功：HTTP 2xx + JSON，业务结果在 `output`（服务接口）或 `data`（文件接口）；
//! - 失败：非 2xx + `{code, message, request_id}`。
//!
//! 本模块只放被多个 API 客户端共享的部分；各接口的请求构造仍在各自模块里。

use anyhow::{Context, Result};
use reqwest::Response;
use serde_json::Value;

/// 百炼 HTTP 接口返回的业务错误（保留状态码与错误码，便于按类型判断）。
#[derive(Debug)]
pub struct ApiError {
    pub action: String,
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    /// 资源不存在：音色/热词表被删除或从未创建。
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

/// 兼容 `output`（服务接口）与 `data`（文件接口）两种响应包裹。
pub fn payload(value: &Value) -> &Value {
    value
        .get("output")
        .filter(|node| node.is_object())
        .or_else(|| value.get("data").filter(|node| node.is_object()))
        .unwrap_or(value)
}

/// 解析响应：非 2xx 时返回带状态码/错误码的 [`ApiError`]。
pub async fn parse_response(response: Response, action: &str) -> Result<Value> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payload_accepts_output_and_data_wrappers() {
        assert_eq!(payload(&json!({"output":{"a":1}}))["a"], 1);
        assert_eq!(payload(&json!({"data":{"b":2}}))["b"], 2);
        assert_eq!(payload(&json!({"c":3}))["c"], 3);
        // output 为 null 时应回退到原始响应
        assert_eq!(payload(&json!({"output": null, "c": 3}))["c"], 3);
    }

    #[test]
    fn resource_missing_detection_covers_known_bailian_shapes() {
        let missing = anyhow::Error::new(ApiError {
            action: "查询".to_string(),
            status: 400,
            code: "BadRequest.ResourceNotExist".to_string(),
            message: "The Required resource not exist".to_string(),
        });
        assert!(is_resource_missing_error(&missing));

        let other = anyhow::Error::new(ApiError {
            action: "查询".to_string(),
            status: 500,
            code: "InternalError".to_string(),
            message: "boom".to_string(),
        });
        assert!(!is_resource_missing_error(&other));
    }
}
