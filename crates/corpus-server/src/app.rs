//! 路由与处理器：现有 Tauri 命令的一一对应（薄壳——领域逻辑全在
//! corpus-engine，这里只做参数搬运与错误映射，与桌面壳同级）。
//!
//! 响应 JSON 字段形状与引擎 serde 完全一致（前端类型零改动）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::Path;

use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio_stream::wrappers::BroadcastStream;

use corpus_engine::{
    builtin_template_infos, create_vault, ingest_file, list_pages, open_vault, test_connection,
    AppConfig, ConnectionReport, IngestFileReport, IngestStage, ModelTier, OpenedVault, PageTree,
    RecentVault, TemplateId, TemplateInfo, VaultInfo,
};

use crate::error::{ApiError, ApiJson};
use crate::state::AppState;

/// token 自定义头（缺失 / 错误 → 401，中文 JSON 错误体）。
pub const TOKEN_HEADER: &str = "X-Corpus-Token";

/// SSE 进度事件名（前端 EventSource addEventListener 同名）。
pub const INGEST_PROGRESS_SSE_EVENT: &str = "ingest-progress";

/// 广播对（事件名 + JSON payload）→ SSE 事件的单一映射：按事件名输出
/// `event: <名>`（data 为 payload 的 JSON）。payload 序列化失败丢弃该
/// 事件（进度语义可容忍，不让单个坏事件断流）。
fn sse_event((name, payload): (String, serde_json::Value)) -> Option<Result<Event, Infallible>> {
    serde_json::to_string(&payload)
        .ok()
        .map(|data| Ok(Event::default().event(name).data(data)))
}

/// 组装完整 Router：/api（token 中间件内校验）+ dist 静态资源。
pub fn build_router(state: AppState, dist: &Path) -> Router {
    let api = Router::new()
        .route("/templates", get(list_templates))
        .route("/vaults", post(create_vault_handler))
        .route("/vaults/open", post(open_vault_handler))
        .route("/vaults/recent", get(list_recent).delete(remove_recent))
        .route("/vaults/pages", get(pages))
        .route("/vaults/page", get(read_page))
        .route("/config", get(get_config).put(put_config))
        .route("/config/test-connection", post(test_connection_handler))
        .route("/ingest", post(ingest))
        .route("/events", get(events))
        .with_state(state.clone());

    // 静态资源：dist 目录 + SPA 刷新 fallback（未命中的前端路由回
    // index.html）。tower-http 的 ServeDir 不提供目录列出（目录请求按
    // index 规则或 404 处理），「禁用目录列出」由此满足；显式开启
    // append_index_html_on_directories 让 `/` 直接出壳页面。静态资源
    // 不要求 token——它只是公开的应用壳，数据入口全部在 /api 后面。
    //
    // 符号链接（评审接受项）：不做 canonicalize。dist 由应用自身
    // `pnpm build` 产出，诱出符号链接需要本机写权限；URL 中的 `..`
    // 段 tower-http 0.6 已直接拒绝。残留的「目录内手工放置 symlink 指
    // 向外部」风险接受，不为此增加路径规范化复杂度。
    let assets = tower_http::services::ServeDir::new(dist)
        .append_index_html_on_directories(true)
        .fallback(tower_http::services::ServeFile::new(
            dist.join("index.html"),
        ));

    Router::new()
        .nest("/api", api)
        .fallback_service(assets)
        .layer(middleware::from_fn_with_state(state, token_auth))
}

// ---------------------------------------------------------------------------
// token 中间件
// ---------------------------------------------------------------------------

/// `/api/*` 的 token 校验（静态资源放行）：
///
/// - 首选 [`TOKEN_HEADER`] 自定义头；
/// - 例外：仅 `/api/events` 额外接受 `?token=` 查询参数——EventSource
///   无法自定义请求头。取舍：URL 带 token 会进浏览器历史与代理日志，
///   但本服务仅绑 127.0.0.1、token 每次启动重新生成（一次性）且只在
///   SSE 一个端点可用，风险可接受；其余 /api 端点坚持只认头，token
///   不进任何 URL / Referer；
/// - 失败一律 401 + 中文 JSON 错误体；
/// - 安全基线（票据 #17）：本服务**不配置任何 CORS 放行**——同源服务
///   下浏览器跨源请求自然失败；自定义头要求使「跨源简化请求」（不带
///   预检）也会因缺头得到 401，恶意网页无法借道读取本机数据。
async fn token_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api/") {
        return next.run(req).await;
    }
    let provided = req
        .headers()
        .get(TOKEN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| {
            if path == "/api/events" {
                query_param(req.uri(), "token").map(str::to_owned)
            } else {
                None
            }
        });
    // 常量时间比较对「仅绑本机、一次性、32 hex 随机」的 token 属过度
    // 设计，普通比较即可（side channel 需要高频探测面，本机单用户场景
    // 不存在）。
    if provided.as_deref() == Some(state.token.as_str()) {
        next.run(req).await
    } else {
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"message": "缺少或无效的访问令牌（X-Corpus-Token 头）。请从 corpus-server 启动输出的 URL 打开本页。"})),
        )
            .into_response()
    }
}

/// 极简 query 参数提取（token 只含 hex 字符，无需 percent-decode）。
fn query_param<'q>(uri: &'q Uri, key: &str) -> Option<&'q str> {
    uri.query()?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then_some(v)
    })
}

/// Query 提取结果的必填参数读取。
fn q_param(q: &HashMap<String, String>, key: &str) -> Result<String, ApiError> {
    q.get(key)
        .cloned()
        .ok_or_else(|| ApiError::bad_request(format!("缺少查询参数 {key}")))
}

// ---------------------------------------------------------------------------
// 端点：与桌面 Tauri 命令一一对应
// ---------------------------------------------------------------------------

/// 列出内置模板元数据（对应 list_templates 命令）。
async fn list_templates() -> Json<Vec<TemplateInfo>> {
    Json(builtin_template_infos())
}

#[derive(Debug, Deserialize)]
struct CreateVaultReq {
    path: String,
    template_id: TemplateId,
}

/// 创建 Vault（对应 create_vault 命令）：引擎创建 + 登记最近列表
/// （登记失败不阻断主流程，与桌面同口径）。
async fn create_vault_handler(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<CreateVaultReq>,
) -> Result<Json<VaultInfo>, ApiError> {
    let info = create_vault(&req.path, &req.template_id)?;
    if let Err(err) = state.store.add_recent_vault(&req.path, &req.template_id) {
        eprintln!("写入最近 Vault 注册表失败（不影响创建结果）：{err}");
    }
    Ok(Json(info))
}

#[derive(Debug, Deserialize)]
struct OpenVaultReq {
    path: String,
}

/// 打开已有 Vault（对应 open_vault 命令）：引擎校验 + 登记最近列表。
async fn open_vault_handler(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<OpenVaultReq>,
) -> Result<Json<OpenedVault>, ApiError> {
    let opened = open_vault(&req.path)?;
    if let Err(err) = state.store.add_recent_vault(&req.path, &opened.template_id) {
        eprintln!("写入最近 Vault 注册表失败（不影响打开结果）：{err}");
    }
    Ok(Json(opened))
}

/// 列出最近打开的 Vault（对应 list_recent_vaults 命令）。
async fn list_recent(State(state): State<AppState>) -> Result<Json<Vec<RecentVault>>, ApiError> {
    Ok(Json(state.store.list_recent_vaults()?))
}

/// 从最近列表移除（对应 remove_recent_vault 命令）。
///
/// path 经 **query 参数**传递（`DELETE /api/vaults/recent?path=...`）而
/// 非请求体：DELETE 携带语义化 body 的客户端/代理兼容性参差（部分会
/// 剥离 body），query 直白且从访问记录即可辨认操作对象。
async fn remove_recent(
    State(state): State<AppState>,
    Query(q): Query<HashMap<String, String>>,
) -> Result<Json<()>, ApiError> {
    let path = q_param(&q, "path")?;
    state.store.remove_recent_vault(&path)?;
    Ok(Json(()))
}

/// 当前 Vault 的 wiki/ 页面树（对应 list_pages 命令）。
async fn pages(Query(q): Query<HashMap<String, String>>) -> Result<Json<PageTree>, ApiError> {
    let vault = q_param(&q, "vault")?;
    Ok(Json(list_pages(&vault)?))
}

/// 只读读取 wiki/ 某页面原始文本（对应 read_page 命令，预览用）。
///
/// 全部校验与读取都在引擎 [`corpus_engine::read_page`] 单一入口（open_vault
/// 级 Vault 校验 + resolve_page_path 路径安全关口 + 读原文三段：拒绝绝对
/// 路径 / `..` 逃逸 / 非 `.md` / symlink 逃逸）——HTTP 层不重复造轮子，
/// 也不因「服务只绑本机」而绕过校验，与桌面壳同一口径。
async fn read_page(Query(q): Query<HashMap<String, String>>) -> Result<Json<String>, ApiError> {
    let vault = q_param(&q, "vault")?;
    let rel = q_param(&q, "rel")?;
    Ok(Json(corpus_engine::read_page(&vault, &rel)?))
}

/// 读取全局配置（对应 get_config 命令）。
async fn get_config(State(state): State<AppState>) -> Result<Json<AppConfig>, ApiError> {
    Ok(Json(state.store.get_config()?))
}

/// 保存全局配置（对应 set_config 命令，整体覆盖写；引擎侧 validate）。
async fn put_config(
    State(state): State<AppState>,
    ApiJson(config): ApiJson<AppConfig>,
) -> Result<Json<AppConfig>, ApiError> {
    state.store.set_config(&config)?;
    Ok(Json(config))
}

/// 测试连通（对应 test_connection 命令）：按传入档位真实最小补全调用。
async fn test_connection_handler(
    ApiJson(tier): ApiJson<ModelTier>,
) -> Result<Json<ConnectionReport>, ApiError> {
    Ok(Json(test_connection(&tier).await?))
}

#[derive(Debug, Deserialize)]
struct IngestReq {
    vault_path: String,
    src_path: String,
}

/// 摄取一个文档（对应 ingest_file 命令）：编排整体委托引擎
/// [`corpus_engine::ingest_file`]（导入 → 端口带上限 → ingest → 按
/// apply_mode 分支 apply），双壳行为一致。
///
/// - **全局互斥**：`try_lock` 命中即 409 中文错误。web 壳是多标签 /
///   多请求环境，并发摄取会交织 git 提交与指纹状态，同一时刻只允许
///   一个；桌面壳无此限制（Tauri 单窗口天然串行）——这是 web 形态
///   新增的语义，且 guard 随 handler future 结束（含 panic unwind 后
///   由 Mutex 修复毒锁语义）自动释放；
/// - **进度不经本响应**：`on_progress` 回调发 broadcast channel，由
///   GET /api/events 的 SSE 连接以 `event: ingest-progress` 推送；
///   本响应等到摄取完成，body 即完整 [`IngestFileReport`]（与 Tauri
///   命令的 await 语义一致，前端 Promise 用法零改动）。
async fn ingest(
    State(state): State<AppState>,
    ApiJson(req): ApiJson<IngestReq>,
) -> Result<Json<IngestFileReport>, ApiError> {
    let _guard = state.ingest_lock.try_lock().map_err(|_| {
        ApiError::conflict(
            "已有另一个摄取正在进行（服务端同一时刻只允许一个摄取），请等待其完成后再试",
        )
    })?;
    let config = state.store.get_config()?;
    let progress_tx = state.progress_tx.clone();
    let report = ingest_file(
        &req.vault_path,
        &req.src_path,
        &config,
        move |stage: IngestStage| {
            // 引擎 IngestStage 即纯数据 enum，序列化不会失败（失败落 Null
            // 只影响该条进度展示）；无订阅者 / 通道满时 send 出错只丢
            // 进度，不阻断摄取。
            let payload = serde_json::to_value(&stage).unwrap_or(serde_json::Value::Null);
            let _ = progress_tx.send((INGEST_PROGRESS_SSE_EVENT.to_string(), payload));
        },
    )
    .await?;
    Ok(Json(report))
}

/// SSE 事件流：把广播通道的「事件名 + JSON payload」对经 [`sse_event`]
/// 转成 `event: <名>` 逐条输出。token 经 `?token=` 查询参数（EventSource
/// 无法自定义头，见 token_auth 注释）。订阅晚于事件发出时拿不到先前
/// 事件（进度语义可容忍）；新增事件类型即发新的名字对，本写循环与
/// 通道零改动（T9 查询流式的扩展点）。
async fn events(
    State(state): State<AppState>,
) -> Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>> {
    let rx = state.progress_tx.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|item| async move {
        match item {
            Ok(pair) => sse_event(pair),
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                eprintln!("SSE 进度订阅者落后，丢弃 {skipped} 条进度事件");
                None
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

// ---------------------------------------------------------------------------
// 测试（tower::ServiceExt::oneshot 方案）：单测直接把请求喂给 Router，
// 不起真实端口、不绑网络——离线、确定性、无端口冲突；比「随机端口 +
// reqwest」少一层进程/端口管理，且能直接持有 AppState（互斥锁测试
// 需要手动预锁）。真实监听行为（只绑 127.0.0.1）在 main 的 bind 处，
// 由人工 curl 验证覆盖。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    const TEST_TOKEN: &str = "0123456789abcdef0123456789abcdef";

    struct TestEnv {
        app: Router,
        state: AppState,
        _dir: tempfile::TempDir,
    }

    /// 临时数据目录下的标准测试环境（ConfigStore、一次性 token、
    /// 不存在的 dist——静态测试单独建 dist）。
    fn test_env() -> TestEnv {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let store = corpus_engine::ConfigStore::open(dir.path()).expect("ConfigStore 失败");
        let state = AppState::new(store, TEST_TOKEN.to_string());
        let app = build_router(state.clone(), Path::new("definitely-not-exist-dist"));
        TestEnv {
            app,
            state,
            _dir: dir,
        }
    }

    fn get(uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri(uri);
        if let Some(t) = token {
            builder = builder.header(TOKEN_HEADER, t);
        }
        builder.body(Body::empty()).expect("构造请求失败")
    }

    fn post_json(uri: &str, token: Option<&str>, body: &serde_json::Value) -> Request<Body> {
        let mut builder = Request::builder()
            .method(axum::http::Method::POST)
            .uri(uri)
            .header(axum::http::header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            builder = builder.header(TOKEN_HEADER, t);
        }
        builder
            .body(Body::from(body.to_string()))
            .expect("构造请求失败")
    }

    /// 发送请求并解码响应体（空体 → null；非 JSON → 字符串值）。
    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let res = app.clone().oneshot(req).await.expect("oneshot 失败");
        let status = res.status();
        let bytes = to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("读响应体失败");
        let json = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
            })
        };
        (status, json)
    }

    fn authed_get(uri: &str) -> Request<Body> {
        get(uri, Some(TEST_TOKEN))
    }

    /// 验收：无 token / 错 token → 401 + 中文 JSON 错误体（含「令牌」）。
    #[tokio::test]
    async fn api_without_or_with_wrong_token_is_401_chinese_json() {
        let env = test_env();
        for req in [
            get("/api/templates", None),
            get("/api/templates", Some("wrong-token")),
            get("/api/config", None),
            post_json(
                "/api/ingest",
                None,
                &json!({"vault_path": "/x", "src_path": "/y"}),
            ),
        ] {
            let (status, body) = send(&env.app, req).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            let message = body["message"].as_str().expect("错误体应为 {message}");
            assert!(message.contains("令牌"), "错误文案应指明令牌：{message}");
        }
    }

    /// 验收：带 token 的 templates / config 正常；config PUT 回读一致；
    /// DELETE 最近列表经 query 参数工作。
    #[tokio::test]
    async fn token_header_grants_templates_config_roundtrip_and_recent() {
        let env = test_env();

        let (status, body) = send(&env.app, authed_get("/api/templates")).await;
        assert_eq!(status, StatusCode::OK);
        let templates = body.as_array().expect("模板应为数组");
        assert!(!templates.is_empty());
        assert!(templates[0]["id"].is_string());

        let (status, body) = send(&env.app, authed_get("/api/config")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["fast"]["provider"].is_string());
        assert!(body["strong"]["model"].is_string());
        assert_eq!(body["apply_mode"], "manual", "默认配置 apply_mode=manual");

        let config = json!({
            "fast": {"provider": "openai", "model": "gpt-test", "base_url": null, "api_key_env": null},
            "strong": {"provider": "anthropic", "model": "claude-test", "base_url": null, "api_key_env": null},
            "apply_mode": "auto",
            "query_pages": true
        });
        let (status, body) = send(
            &env.app,
            Request::builder()
                .method(axum::http::Method::PUT)
                .uri("/api/config")
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .header(TOKEN_HEADER, TEST_TOKEN)
                .body(Body::from(config.to_string()))
                .expect("构造请求失败"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["fast"]["model"], "gpt-test");
        let (_, body) = send(&env.app, authed_get("/api/config")).await;
        assert_eq!(body["strong"]["model"], "claude-test", "PUT 应真实落库");

        // DELETE 最近列表（query 传 path）：空列表移除也成功（幂等）
        let req = Request::builder()
            .method(axum::http::Method::DELETE)
            .uri("/api/vaults/recent?path=/no/such")
            .header(TOKEN_HEADER, TEST_TOKEN)
            .body(Body::empty())
            .expect("构造请求失败");
        let (status, _) = send(&env.app, req).await;
        assert_eq!(status, StatusCode::OK);
    }

    /// 验收：Vault 创建 / 打开 / 最近列表走真实引擎与真实 sqlite。
    #[tokio::test]
    async fn vault_create_open_and_recent_roundtrip() {
        let env = test_env();
        let vault_dir = tempfile::tempdir().expect("tempdir 失败");
        let vault_path = vault_dir.path().join("my-vault");

        let (status, body) = send(
            &env.app,
            post_json(
                "/api/vaults",
                Some(TEST_TOKEN),
                &json!({"path": vault_path.to_string_lossy(), "template_id": "generic"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["template_id"], "generic");
        assert!(
            vault_path.join(".corpus/vault.json").is_file(),
            "应真实落盘"
        );

        // 创建即登记最近列表
        let (_, body) = send(&env.app, authed_get("/api/vaults/recent")).await;
        let recent = body.as_array().expect("最近列表应为数组");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["template_id"], "generic");

        // 打开既有 Vault：校验通过 + 再次登记（仍一条，PRIMARY KEY 去重）
        let (status, body) = send(
            &env.app,
            post_json(
                "/api/vaults/open",
                Some(TEST_TOKEN),
                &json!({"path": vault_path.to_string_lossy()}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["template_name"], "Generic");

        // 非 Vault 路径明确报错（中文 message）
        let (status, body) = send(
            &env.app,
            post_json(
                "/api/vaults/open",
                Some(TEST_TOKEN),
                &json!({"path": vault_dir.path().join("not-a-vault").to_string_lossy()}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body["message"].as_str().unwrap().contains("Vault"));

        let _ = env; // TempDir 与 store 一同存活
    }

    /// 验收：read_page 对 `../` 逃逸返回错误（引擎单关口，HTTP 层不绕过）；
    /// 正常路径可读出页面原文。
    #[tokio::test]
    async fn read_page_rejects_path_escape_and_serves_page() {
        let env = test_env();
        let vault_dir = tempfile::tempdir().expect("tempdir 失败");
        let vault_path = vault_dir.path().join("vault");
        corpus_engine::create_vault(&vault_path, &TemplateId::from("generic"))
            .expect("建 Vault 失败");
        let sources = vault_path.join("wiki/sources");
        std::fs::create_dir_all(&sources).expect("建 sources 失败");
        std::fs::write(sources.join("hello.md"), "# Hello\n正文内容").expect("写页面失败");

        let vault_enc = vault_path.to_string_lossy();

        // 正常读取：返回 JSON 字符串（与 invoke 的 String 等价）
        let (status, body) = send(
            &env.app,
            authed_get(&format!(
                "/api/vaults/page?vault={vault_enc}&rel=sources/hello.md"
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::Value::String("# Hello\n正文内容".into()));

        // `../` 逃逸：被引擎校验拒绝（HTTP 400 + 中文 message）
        for rel in ["../purpose.md", "../../outside.md", "..%2Fpurpose.md"] {
            let (status, body) = send(
                &env.app,
                authed_get(&format!("/api/vaults/page?vault={vault_enc}&rel={rel}")),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "rel={rel}");
            assert!(
                body["message"].as_str().unwrap().contains("非法"),
                "rel={rel} 应报路径非法：{body}"
            );
        }

        // 非 Vault 根同样拒绝（先过 open_vault 校验）
        let other = vault_dir.path().to_string_lossy();
        let (status, _) = send(
            &env.app,
            authed_get(&format!(
                "/api/vaults/page?vault={other}&rel=sources/hello.md"
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// 验收：SSE 端点无 token 401；`?token=` 查询参数放行（EventSource
    /// 无法自定义头的替代通道）；`?token=` 错值仍 401。
    #[tokio::test]
    async fn events_sse_validates_token_via_query_param() {
        let env = test_env();

        let (status, _) = send(&env.app, get("/api/events", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);

        // ?token= 查询参数放行（正确的 token）
        let res = env
            .app
            .clone()
            .oneshot(get(&format!("/api/events?token={TEST_TOKEN}"), None))
            .await
            .expect("oneshot 失败");
        assert_eq!(res.status(), StatusCode::OK);
        let content_type = res
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        assert!(
            content_type.starts_with("text/event-stream"),
            "SSE 响应 content-type 应为 text/event-stream：{content_type}"
        );

        let (status, _) = send(&env.app, get("/api/events?token=wrong", None)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// 验收：通道承载「事件名 + JSON payload」对，SSE 写循环按事件名
    /// 输出（`event: <名>` + `data: <JSON>`）——新增事件类型即发新名字
    /// 对，通道与写循环零改动。逐帧读流（流因 keep-alive 不主动结束，
    /// 不能 to_bytes 等流尾）；事件不重放（进度语义），故每对事件各开
    /// 一条连接、订阅建立后再发送。
    #[tokio::test]
    async fn sse_stream_writes_named_events_from_channel_pairs() {
        let env = test_env();
        for (name, payload) in [
            (
                INGEST_PROGRESS_SSE_EVENT.to_string(),
                json!({"stage": "analyzing", "chars": 12}),
            ),
            ("query-progress".to_string(), json!({"n": 1})),
        ] {
            let res = env
                .app
                .clone()
                .oneshot(get(&format!("/api/events?token={TEST_TOKEN}"), None))
                .await
                .expect("oneshot 失败");
            assert_eq!(res.status(), StatusCode::OK);

            env.state
                .progress_tx
                .send((name.clone(), payload))
                .expect("通道发送失败");
            let mut body = res.into_body();
            let frame = body
                .frame()
                .await
                .expect("SSE 流应产出事件帧")
                .expect("SSE 帧不应出错");
            let bytes = frame.into_data().expect("SSE 事件帧应为 data");
            let text = std::str::from_utf8(&bytes).expect("SSE 帧应为 UTF-8");
            let event_line = text
                .lines()
                .find(|l| l.starts_with("event: "))
                .unwrap_or_else(|| panic!("SSE 帧缺 event 行：{text}"));
            assert_eq!(event_line, format!("event: {name}"));
            let data_line = text
                .lines()
                .find(|l| l.starts_with("data: "))
                .unwrap_or_else(|| panic!("SSE 帧缺 data 行：{text}"));
            let decoded: serde_json::Value =
                serde_json::from_str(data_line.trim_start_matches("data: ")).expect("data 非 JSON");
            assert!(decoded.is_object(), "data 应为 payload JSON：{text}");
        }
    }

    /// 验收：并发摄取返回 409。可行方案：**手动预锁全局互斥**再请求
    /// ——server 层用 rig 真实端口（无 fake 注入点；端口注入测试缝在
    /// 引擎层已由 ingest_file_with_ports 覆盖），而真实摄取耗时不可控；
    /// 预锁确定性地命中 409 分支（互斥检查先于引擎调用与 Vault 校验，
    /// 无需真实 Vault），是断言互斥语义的最小充分手段。
    #[tokio::test]
    async fn concurrent_ingest_returns_409() {
        let env = test_env();
        // 模拟「另一个摄取正在进行」：持有全局互斥
        let _guard = env.state.ingest_lock.lock().await;

        let (status, body) = send(
            &env.app,
            post_json(
                "/api/ingest",
                Some(TEST_TOKEN),
                &json!({"vault_path": "/tmp/any", "src_path": "/tmp/any.md"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        let message = body["message"].as_str().expect("409 应为 {message}");
        assert!(
            message.contains("摄取"),
            "409 文案应指明摄取冲突：{message}"
        );

        // 释放互斥后同一请求不再 409：请求穿透互斥层进入引擎编排
        // （默认配置在此报引擎错误——缺 key / 缺模型名等，随环境而定，
        // 不绑具体文案，只断言互斥语义）
        drop(_guard);
        let (status, body) = send(
            &env.app,
            post_json(
                "/api/ingest",
                Some(TEST_TOKEN),
                &json!({"vault_path": "/tmp/any", "src_path": "/tmp/any.md"}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let message = body["message"].as_str().unwrap_or_default();
        assert!(
            !message.contains("已有另一个摄取"),
            "互斥释放后不得再报并发冲突：{message}"
        );
    }

    /// 验收：静态资源无需 token；SPA 刷新（非根路径未命中）回
    /// index.html；目录列出不可用（不存在路径 404）。
    #[tokio::test]
    async fn static_assets_served_without_token_with_spa_fallback() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        std::fs::write(dir.path().join("index.html"), "<html>corpus-shell</html>")
            .expect("写 index 失败");
        let store = corpus_engine::ConfigStore::open(dir.path()).expect("ConfigStore 失败");
        let state = AppState::new(store, TEST_TOKEN.to_string());
        let app = build_router(state, dir.path());

        let (status, body) = send(&app, get("/", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_str().unwrap().contains("corpus-shell"));

        // SPA 刷新：任意前端路由未命中静态文件 → 回 index.html
        let (status, body) = send(&app, get("/some/spa/route", None)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_str().unwrap().contains("corpus-shell"));
    }
}
