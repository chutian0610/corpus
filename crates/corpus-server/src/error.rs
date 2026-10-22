//! HTTP 错误统一映射：引擎错误与请求体解析失败 → `{"message": "中文文案"}`
//! JSON 错误体（与 Tauri 命令的 `Err(String)` 展示语义一致，前端
//! errorMessage 直接可用）。

use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::de::DeserializeOwned;
use serde_json::json;

/// API 错误：状态码 + 中文 message。
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    /// 领域校验失败 / 资源不存在等常规错误。
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    /// 路径不存在 / 不是目录等「找不到」语义。
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    /// 请求越界（如目录浏览越出用户主目录）。
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    /// 服务端环境问题（无法定位系统目录等），请求本身无错。
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }

    /// 并发摄取冲突（全局互斥命中，见 POST /api/ingest 注释）。
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }
}

impl From<corpus_engine::Error> for ApiError {
    fn from(err: corpus_engine::Error) -> Self {
        Self::bad_request(err.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "message": self.message }))).into_response()
    }
}

/// 自定义 JSON 提取器：请求体不合法时也返回统一中文 JSON 错误体
/// （axum 内建 `Json<T>` 的 rejection 是英文纯文本，前端无法统一展示）。
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError::bad_request(format!(
                "请求体 JSON 无效：{rejection}"
            ))),
        }
    }
}
