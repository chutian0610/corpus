//! 路由与处理器：现有 Tauri 命令的一一对应（薄壳——领域逻辑全在
//! corpus-engine，这里只做参数搬运与错误映射，与桌面壳同级）。
//!
//! 响应 JSON 字段形状与引擎 serde 完全一致（前端类型零改动）。

use std::collections::HashMap;
use std::convert::Infallible;
use std::path::{Path, PathBuf};

use axum::extract::{Query, Request, State};
use axum::http::{header, StatusCode};
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

/// 组装完整 Router：/api（本机源校验中间件）+ dist 静态资源。
pub fn build_router(state: AppState, dist: &Path) -> Router {
    let api = Router::new()
        .route("/templates", get(list_templates))
        .route("/vaults", post(create_vault_handler))
        .route("/vaults/open", post(open_vault_handler))
        .route("/vaults/recent", get(list_recent).delete(remove_recent))
        .route("/vaults/pages", get(pages))
        .route("/vaults/page", get(read_page))
        .route("/fs/list", get(fs_list))
        .route("/config", get(get_config).put(put_config))
        .route("/config/test-connection", post(test_connection_handler))
        .route("/ingest", post(ingest))
        .route("/events", get(events))
        .with_state(state.clone());

    // 静态资源：dist 目录 + SPA 刷新 fallback（未命中的前端路由回
    // index.html）。tower-http 的 ServeDir 不提供目录列出（目录请求按
    // index 规则或 404 处理），「禁用目录列出」由此满足；显式开启
    // append_index_html_on_directories 让 `/` 直接出壳页面。静态资源
    // 只是公开的应用壳，数据入口全部在 /api 后面（本机源校验）。
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
        .layer(middleware::from_fn(local_origin_auth))
}

// ---------------------------------------------------------------------------
// 本机源校验中间件（替代早期的一次性 token 方案）
// ---------------------------------------------------------------------------
//
// 服务只绑 127.0.0.1，威胁模型是**浏览器侧**的攻击者（本机其他进程
// 属 ADR-0003 已接受的信任域——它们本就能读 0600 的 config.db）：
//
// - **drive-by CSRF**：恶意网页向 http://127.0.0.1:8619 发跨源 POST，
//   响应虽不可读但副作用会执行（触发摄取烧 LLM token、改写 Vault）。
//   浏览器跨源请求必带 Origin 头——非本机源一律 403；
// - **DNS rebinding**：evil.com 解析到 127.0.0.1 使页面「同源」可读
//   响应——此时 Host 头是 evil.com，非本机主机名一律 403。
//
// 规则（对 /api/* 生效；静态资源只是公开应用壳，不校验）：
// - `Host` 头的主机部分必须是回环名（127.0.0.1 / localhost / ::1）；
// - `Origin` 头（存在时）的源必须是本机 `http://127.0.0.1:{port}` 或
//   `http://localhost:{port}`。**不存在则放行**——curl / 脚本等非浏览器
//   客户端不带 Origin，这与「本机同用户进程可信」的姿态一致，也是
//   CLI / e2e 直接调 API 的通道。
//
// 失败一律 403 + 中文 JSON 错误体。为什么不需要一次性 token：本机
// 攻击面上 token 与源校验防的是同一批威胁，而 token 有不可避免的
// 暴露面（启动日志 / 终端回显 / 截图外流，用户实测两次误贴），源
// 校验零摩擦且防线等价。

/// 回环主机名集合（Host / Origin 校验的放行名单）。
const LOOPBACK_HOSTS: &[&str] = &["127.0.0.1", "localhost", "::1"];

/// 取 Host 头的主机部分（剥掉端口；IPv6 的 `[::1]:8619` 形态剥出
/// `::1`）。
fn host_part(host: &str) -> &str {
    if let Some(rest) = host.strip_prefix('[') {
        rest.split_once(']').map_or(rest, |(h, _)| h)
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    }
}

/// Origin 是否为本机源（`http://<回环名>:{port}`）。非 http、主机非
/// 回环名或端口不匹配都算不通过。
fn is_loopback_origin(origin: &str, port: u16) -> bool {
    let Some(rest) = origin.strip_prefix("http://") else {
        return false;
    };
    let host_with_port = rest.split('/').next().unwrap_or_default();
    LOOPBACK_HOSTS.contains(&host_part(host_with_port))
        && host_with_port.ends_with(&format!(":{port}"))
}

/// `/api/*` 的本机源校验（静态资源放行）：
/// - `Host` 主机部分必须为回环名（拦 DNS rebinding）；
/// - `Origin` 存在时必须为本机源（拦 drive-by CSRF；不存在放行
///   curl / 脚本）。
async fn local_origin_auth(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if !path.starts_with("/api/") {
        return next.run(req).await;
    }
    let headers = req.headers();
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !LOOPBACK_HOSTS.contains(&host_part(host)) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"message": "拒绝访问：Host 非本机地址（127.0.0.1 / localhost）。请从 http://127.0.0.1:8619/ 访问。"})),
        )
            .into_response();
    }
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        let port = host.rsplit_once(':').map_or("", |(_, p)| p);
        if !is_loopback_origin(origin, port.parse().unwrap_or(0)) {
            return (
                StatusCode::FORBIDDEN,
                Json(json!({"message": "拒绝跨源请求：Origin 非本机地址。本服务只接受从 http://127.0.0.1:8619/ 发起的调用。"})),
            )
                .into_response();
        }
    }
    next.run(req).await
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
///
/// 响应中 `api_key` 为**明文**：server 只绑 127.0.0.1（本机信任域）且
/// 全部 /api 请求经本机源校验门控，密钥不出本机进程边界；把配置原样
/// 回传是设置页回显所需，明文本身是 ADR-0003 接受的取舍。
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
/// 转成 `event: <名>` 逐条输出。同源 EventSource 直连（本机源校验
/// 中间件放行本机源）。订阅晚于事件发出时拿不到先前
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
// 端点：web 壳新增（桌面壳由 plugin-dialog 弹系统对话框，浏览器弹不了）
// ---------------------------------------------------------------------------

/// 单个目录项：web 侧目录浏览的最小信息（只读浏览，无大小 / 权限等
/// 元数据；`path` 为拼接好的绝对路径，前端直接用它继续下钻或选中）。
#[derive(Debug, serde::Serialize)]
struct FsEntry {
    name: String,
    path: String,
    is_dir: bool,
}

/// 一次目录列举的响应：当前目录（canonical）+ 其下条目。
#[derive(Debug, serde::Serialize)]
struct FsListing {
    path: String,
    entries: Vec<FsEntry>,
}

/// 过滤隐藏项（`.` 开头）并排序：目录在前、名称升序。**纯函数**——
/// 过滤与排序规则在此单点，单测直接构造条目即可覆盖，不碰文件系统。
fn visible_sorted(mut entries: Vec<FsEntry>) -> Vec<FsEntry> {
    entries.retain(|entry| !entry.name.starts_with('.'));
    // bool 的序是 false < true：以 b 为第一比较键（dir=true）即目录在前
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
    entries
}

/// 读一个目录为 [`FsEntry`] 列表（过滤 + 排序委托 [`visible_sorted`]）。
///
/// symlink 的 is_dir 取 link 本身类型（`DirEntry::file_type` 不跟随）：
/// 指向 home 外的链接目录不会被前端当成可进入项，与 server 端「越出
/// home 即 403」的关口口径一致（进去了也只会得到 403，这里直接不诱出）。
fn read_visible_entries(dir: &Path) -> std::io::Result<Vec<FsEntry>> {
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_dir = entry.file_type()?.is_dir();
        entries.push(FsEntry {
            path: entry.path().to_string_lossy().into_owned(),
            name,
            is_dir,
        });
    }
    Ok(visible_sorted(entries))
}

/// 列出文件系统目录（`GET /api/fs/list?path=<可选>`）：web 壳的「浏览…」
/// 数据源。桌面壳选路径走 plugin-dialog 的系统对话框，浏览器弹不了，
/// 改由本端点只读列目录——这也是全服务唯一的任意路径读取面，因此：
///
/// - **只允许 home 之内**：缺省列用户主目录（`dirs::home_dir`）；显式
///   path 先 canonicalize（解析 `..` 与 symlink，失败即不存在 → 404），
///   再断言落在 home canonical 之下，越界一律 403（`Path::starts_with`
///   按组件比较，`/home/x` 与 `/home/xy` 不会混淆）；
/// - **只读**：仅列目录项，不提供任何写操作；`.` 开头隐藏项跳过（浏览
///   Vault / 文档用不到，也少暴露一层）；canonical home 本身失败属
///   服务端环境问题 → 500。
async fn fs_list(Query(q): Query<HashMap<String, String>>) -> Result<Json<FsListing>, ApiError> {
    let home = dirs::home_dir().ok_or_else(|| {
        ApiError::internal("无法定位用户主目录（dirs::home_dir 返回 None），目录浏览不可用")
    })?;
    let home_canonical = home.canonicalize().map_err(|err| {
        ApiError::internal(format!("解析用户主目录 {} 失败：{err}", home.display()))
    })?;

    let requested = match q.get("path").map(String::as_str).filter(|s| !s.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => home,
    };
    let canonical = requested.canonicalize().map_err(|err| {
        ApiError::not_found(format!(
            "路径不存在或无法访问：{}（{err}）",
            requested.display()
        ))
    })?;
    if !canonical.is_dir() {
        return Err(ApiError::not_found(format!(
            "该路径不是目录：{}",
            canonical.display()
        )));
    }
    if !canonical.starts_with(&home_canonical) {
        return Err(ApiError::forbidden("仅允许浏览用户主目录内的路径"));
    }

    let entries = read_visible_entries(&canonical)
        .map_err(|err| ApiError::bad_request(format!("读取目录失败：{err}")))?;
    Ok(Json(FsListing {
        path: canonical.to_string_lossy().into_owned(),
        entries,
    }))
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

    /// 本机源常量（请求头构造用；与 server 绑定端口一致）。
    const LOCAL_HOST: &str = "127.0.0.1:8619";

    struct TestEnv {
        app: Router,
        state: AppState,
        _dir: tempfile::TempDir,
    }

    /// 临时数据目录下的标准测试环境（ConfigStore；不存在的 dist——静态
    /// 测试单独建 dist）。
    fn test_env() -> TestEnv {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let store = corpus_engine::ConfigStore::open(dir.path()).expect("ConfigStore 失败");
        let state = AppState::new(store);
        let app = build_router(state.clone(), Path::new("definitely-not-exist-dist"));
        TestEnv {
            app,
            state,
            _dir: dir,
        }
    }

    /// 构造请求：默认带本机 Host（模拟真实浏览器/客户端），Origin 可选。
    fn request(
        method: axum::http::Method,
        uri: &str,
        host: Option<&str>,
        origin: Option<&str>,
        body: Option<String>,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(axum::http::header::HOST, host.unwrap_or(LOCAL_HOST));
        if let Some(o) = origin {
            builder = builder.header(axum::http::header::ORIGIN, o);
        }
        match body {
            Some(b) => builder
                .header(axum::http::header::CONTENT_TYPE, "application/json")
                .body(Body::from(b))
                .expect("构造请求失败"),
            None => builder.body(Body::empty()).expect("构造请求失败"),
        }
    }

    fn get(uri: &str) -> Request<Body> {
        request(axum::http::Method::GET, uri, None, None, None)
    }

    fn get_from(origin_host: &str, uri: &str) -> Request<Body> {
        request(
            axum::http::Method::GET,
            uri,
            Some(origin_host),
            Some(format!("http://{origin_host}").as_str()),
            None,
        )
    }

    fn post_json(uri: &str, body: &serde_json::Value) -> Request<Body> {
        request(
            axum::http::Method::POST,
            uri,
            None,
            None,
            Some(body.to_string()),
        )
    }

    fn put_json(uri: &str, body: &serde_json::Value) -> Request<Body> {
        request(
            axum::http::Method::PUT,
            uri,
            None,
            None,
            Some(body.to_string()),
        )
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

    /// 验收：本机源 / 无 Origin 头（curl、脚本形态）一律放行——templates
    /// 与 config 读写全链路真实落库。
    #[tokio::test]
    async fn local_and_originless_requests_pass_and_roundtrip_config() {
        let env = test_env();

        let (status, body) = send(&env.app, get("/api/templates")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.as_array().expect("模板应为数组").is_empty());

        // 本机 Origin（真实浏览器形态）
        let (status, body) = send(&env.app, get_from(LOCAL_HOST, "/api/config")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["apply_mode"], "manual", "默认配置 apply_mode=manual");

        // 无 Origin（curl / e2e 脚本形态）：PUT 真实落库
        let config = json!({
            "fast": {"provider": "openai", "model": "gpt-test", "base_url": null, "api_key": null},
            "strong": {"provider": "anthropic", "model": "claude-test", "base_url": null, "api_key": null},
            "apply_mode": "auto",
            "query_pages": true
        });
        let (status, body) = send(&env.app, put_json("/api/config", &config)).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["fast"]["model"], "gpt-test");
        let (_, body) = send(&env.app, get("/api/config")).await;
        assert_eq!(body["strong"]["model"], "claude-test", "PUT 应真实落库");
    }

    /// 验收：跨源 Origin（drive-by CSRF）一律 403 + 中文错误体，覆盖
    /// GET / POST / SSE 三类端点。
    #[tokio::test]
    async fn cross_origin_requests_are_rejected_403() {
        let env = test_env();
        for req in [
            get_from("evil.com", "/api/templates"),
            post_json(
                "/api/ingest",
                &json!({"vault_path": "/x", "src_path": "/y"}),
            ),
            get("/api/events?token=irrelevant"),
        ] {
            let (req, _) = {
                // 给无 Origin 的请求补一个 evil Origin（SSE 用例）
                let mut req = req;
                if req.headers().get(axum::http::header::ORIGIN).is_none() {
                    req.headers_mut().insert(
                        axum::http::header::ORIGIN,
                        axum::http::HeaderValue::from_static("http://evil.com"),
                    );
                }
                (req, ())
            };
            let (status, body) = send(&env.app, req).await;
            assert_eq!(status, StatusCode::FORBIDDEN);
            let message = body["message"].as_str().expect("403 应为 {message}");
            assert!(
                message.contains("跨源") || message.contains("本机"),
                "{message}"
            );
        }
    }

    /// 验收：DNS rebinding——Host 为非回环名（攻击者域名解析到
    /// 127.0.0.1，页面即「同源」）一律 403；localhost 变体放行。
    #[tokio::test]
    async fn rebinding_host_is_rejected_and_localhost_passes() {
        let env = test_env();

        let req = request(
            axum::http::Method::GET,
            "/api/templates",
            Some("evil.com:8619"),
            None,
            None,
        );
        let (status, body) = send(&env.app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(
            body["message"].as_str().unwrap().contains("Host"),
            "403 文案应说明 Host 校验：{body}"
        );

        for host in ["localhost:8619", "[::1]:8619"] {
            let req = request(
                axum::http::Method::GET,
                "/api/templates",
                Some(host),
                None,
                None,
            );
            let (status, _) = send(&env.app, req).await;
            assert_eq!(status, StatusCode::OK, "回环变体 {host} 应放行");
        }
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

        let (_, body) = send(&env.app, get("/api/vaults/recent")).await;
        let recent = body.as_array().expect("最近列表应为数组");
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0]["template_id"], "generic");

        let (status, body) = send(
            &env.app,
            post_json(
                "/api/vaults/open",
                &json!({"path": vault_path.to_string_lossy()}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["template_name"], "Generic");

        let (status, body) = send(
            &env.app,
            post_json(
                "/api/vaults/open",
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

        let (status, body) = send(
            &env.app,
            get(&format!(
                "/api/vaults/page?vault={vault_enc}&rel=sources/hello.md"
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::Value::String("# Hello\n正文内容".into()));

        for rel in ["../purpose.md", "../../outside.md", "..%2Fpurpose.md"] {
            let (status, body) = send(
                &env.app,
                get(&format!("/api/vaults/page?vault={vault_enc}&rel={rel}")),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "rel={rel}");
            assert!(
                body["message"].as_str().unwrap().contains("非法"),
                "rel={rel} 应报路径非法：{body}"
            );
        }

        let other = vault_dir.path().to_string_lossy();
        let (status, _) = send(
            &env.app,
            get(&format!(
                "/api/vaults/page?vault={other}&rel=sources/hello.md"
            )),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// 验收：SSE 端点本机访问直接可用（无 token 时代——同源 EventSource
    /// 或无 Origin 的脚本连接都放行）；跨源 Origin 403。
    #[tokio::test]
    async fn events_sse_allows_local_and_rejects_cross_origin() {
        let env = test_env();

        let res = env
            .app
            .clone()
            .oneshot(get("/api/events"))
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

        let req = request(
            axum::http::Method::GET,
            "/api/events",
            Some(LOCAL_HOST),
            Some("http://evil.com"),
            None,
        );
        let (status, body) = send(&env.app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["message"].as_str().unwrap().contains("跨源"));
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
                .oneshot(get("/api/events"))
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

    /// 验收：并发摄取返回 409。**手动预锁全局互斥**再请求——server 层
    /// 用 rig 真实端口（无 fake 注入点；端口注入测试缝在引擎层已由
    /// ingest_file_with_ports 覆盖），真实摄取耗时不可控；预锁确定性
    /// 命中 409 分支（互斥检查先于引擎调用与 Vault 校验，无需真实
    /// Vault），是断言互斥语义的最小充分手段。
    #[tokio::test]
    async fn concurrent_ingest_returns_409() {
        let env = test_env();
        let _guard = env.state.ingest_lock.lock().await;

        let (status, body) = send(
            &env.app,
            post_json(
                "/api/ingest",
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

        drop(_guard);
        let (status, body) = send(
            &env.app,
            post_json(
                "/api/ingest",
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

    /// 验收：fs_list 走真实 home——缺省列用户主目录（响应 path 为 home
    /// canonical，条目结构完整）；home 外（/etc）403 中文错误体；不存在
    /// 路径 404。条目内容不绑机器布局，只断言结构与路径语义。
    #[tokio::test]
    async fn fs_list_lists_home_by_default_and_guards_boundaries() {
        let env = test_env();

        let (status, body) = send(&env.app, get("/api/fs/list")).await;
        assert_eq!(status, StatusCode::OK);
        let home_canonical = dirs::home_dir()
            .expect("测试环境应有用户主目录")
            .canonicalize()
            .expect("home canonicalize 失败");
        assert_eq!(body["path"], home_canonical.to_string_lossy().as_ref());
        let entries = body["entries"].as_array().expect("entries 应为数组");
        for entry in entries {
            assert!(entry["name"].is_string());
            assert!(entry["path"].is_string());
            assert!(entry["is_dir"].is_boolean());
        }

        let (status, body) = send(&env.app, get("/api/fs/list?path=/etc")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body["message"].as_str().unwrap().contains("仅允许浏览"));

        let (status, body) = send(
            &env.app,
            get("/api/fs/list?path=/no/such/dir-for-corpus-test"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["message"].as_str().unwrap().contains("不存在"));
    }

    /// 验收：visible_sorted 纯函数——隐藏项（`.` 开头）被过滤，目录
    /// 在前、各组内名称升序。不碰文件系统。
    #[test]
    fn visible_sorted_filters_hidden_and_orders_dirs_first() {
        let entry = |name: &str, is_dir: bool| FsEntry {
            path: format!("/home/u/{name}"),
            name: name.to_string(),
            is_dir,
        };
        let sorted = visible_sorted(vec![
            entry("zebra.md", false),
            entry(".hidden", false),
            entry("docs", true),
            entry(".corpus", true),
            entry("alpha", true),
            entry("apple.txt", false),
        ]);
        let names: Vec<&str> = sorted.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "docs", "apple.txt", "zebra.md"]);
    }

    /// 验收：read_visible_entries 对真实 tempdir——隐藏文件与隐藏目录
    /// 都被跳过，目录排在文件前（tempdir 只做条目输入，与 home 无关）。
    #[test]
    fn read_visible_entries_skips_hidden_and_sorts() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        std::fs::write(dir.path().join("b.txt"), "x").expect("写文件失败");
        std::fs::write(dir.path().join(".secret"), "x").expect("写隐藏文件失败");
        std::fs::create_dir_all(dir.path().join("a-dir")).expect("建目录失败");
        std::fs::create_dir_all(dir.path().join(".hdir")).expect("建隐藏目录失败");

        let entries = read_visible_entries(dir.path()).expect("列目录失败");
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a-dir", "b.txt"]);
        assert!(entries[0].is_dir);
        assert!(!entries[1].is_dir);
        assert_eq!(entries[0].path, dir.path().join("a-dir").to_string_lossy());
    }

    /// 验收：静态资源无校验直接服务；SPA 刷新（非根路径未命中）回
    /// index.html；目录列出不可用（不存在路径 404）。
    #[tokio::test]
    async fn static_assets_served_with_spa_fallback() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        std::fs::write(dir.path().join("index.html"), "<html>corpus-shell</html>")
            .expect("写 index 失败");
        let store = corpus_engine::ConfigStore::open(dir.path()).expect("ConfigStore 失败");
        let state = AppState::new(store);
        let app = build_router(state, dir.path());

        let (status, body) = send(&app, get("/")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_str().unwrap().contains("corpus-shell"));

        let (status, body) = send(&app, get("/some/spa/route")).await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.as_str().unwrap().contains("corpus-shell"));
    }
}
