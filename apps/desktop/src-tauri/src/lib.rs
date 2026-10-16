//! Corpus 桌面壳：全部领域逻辑在 corpus-engine，这里只做 Tauri 胶水。
//!
//! T2：setup 中解析 `app_data_dir()` 并注入 [`ConfigStore`]（引擎只认
//! 目录参数，见 T2 调研笔记第 6 节），新增配置 / 连通测试 / 最近 Vault
//! 命令。
//! T3：`ingest_file` 命令——引擎进度回调桥接为 `ingest-progress` 事件
//! （AppHandle: Emitter，见 T3 调研笔记第 4 节）。

use corpus_engine::{
    AppConfig, ConfigStore, ConnectionReport, IngestReport, ModelTier, RecentVault, TemplateId,
    TemplateInfo, VaultInfo,
};
use tauri::{AppHandle, Emitter, Manager, State};

/// 前端 listen 的 Ingest 进度事件名。
pub const INGEST_PROGRESS_EVENT: &str = "ingest-progress";

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // 应用数据目录（data_dir/${bundle_identifier}），运行时解析、
            // 不硬编码；目录不存在则创建，随后打开全局 config.db。
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let store = ConfigStore::open(&dir)?;
            app.manage(store);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            create_vault,
            list_templates,
            get_config,
            set_config,
            test_connection,
            list_recent_vaults,
            add_recent_vault,
            remove_recent_vault,
            ingest_file,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 创建 Vault：委托 corpus-engine 的 create_vault 公共 API。
///
/// 创建成功即视为「打开」，写入最近 Vault 注册表（注册失败不阻断主
/// 流程——Vault 已真实存在，只打日志）。
///
/// async + owned 参数（见调研笔记坑清单：引用参数需 Result、主线程不阻塞）；
/// 引擎错误映射为字符串供前端展示（后续票据引入结构化错误序列化）。
#[tauri::command]
async fn create_vault(
    store: State<'_, ConfigStore>,
    path: String,
    template_id: TemplateId,
) -> Result<VaultInfo, String> {
    let info = corpus_engine::create_vault(&path, &template_id).map_err(|e| e.to_string())?;
    if let Err(e) = store.add_recent_vault(&path, &template_id) {
        eprintln!("写入最近 Vault 注册表失败（不影响创建结果）：{e}");
    }
    Ok(info)
}

/// 列出内置模板元数据：前端模板选择器的单一数据源。
///
/// 模板清单与 id 集合由引擎 `builtin_templates` 下发，壳层不硬编码；
/// `TemplateId` serde 透明，前端 JSON 中仍是字符串。
#[tauri::command]
async fn list_templates() -> Vec<TemplateInfo> {
    corpus_engine::builtin_template_infos()
}

/// 读取全局配置；库中无记录时引擎返回默认值。
#[tauri::command]
async fn get_config(store: State<'_, ConfigStore>) -> Result<AppConfig, String> {
    store.get_config().map_err(|e| e.to_string())
}

/// 保存全局配置（整体覆盖写）。API key 只存环境变量名，值不经过本命令。
#[tauri::command]
async fn set_config(store: State<'_, ConfigStore>, config: AppConfig) -> Result<AppConfig, String> {
    store.set_config(&config).map_err(|e| e.to_string())?;
    Ok(config)
}

/// 测试连通：按传入档位对配置端点发起一次真实最小补全调用。
///
/// 未配置模型名 / 缺 API key 环境变量 / 兼容端点缺 base_url 等本地可判定
/// 的失败，引擎会返回中文错误信息（Error::InvalidConfig / MissingApiKey）。
#[tauri::command]
async fn test_connection(tier: ModelTier) -> Result<ConnectionReport, String> {
    corpus_engine::test_connection(&tier)
        .await
        .map_err(|e| e.to_string())
}

/// 列出最近打开的 Vault（按时间降序）。
#[tauri::command]
async fn list_recent_vaults(store: State<'_, ConfigStore>) -> Result<Vec<RecentVault>, String> {
    store.list_recent_vaults().map_err(|e| e.to_string())
}

/// 手动登记一个已存在的 Vault（「打开已有 Vault」入口，后续票据接续）。
#[tauri::command]
async fn add_recent_vault(
    store: State<'_, ConfigStore>,
    path: String,
    template_id: TemplateId,
) -> Result<(), String> {
    store
        .add_recent_vault(&path, &template_id)
        .map_err(|e| e.to_string())
}

/// 从最近列表移除一个 Vault（只动注册表，不删文件）。
#[tauri::command]
async fn remove_recent_vault(store: State<'_, ConfigStore>, path: String) -> Result<(), String> {
    store
        .remove_recent_vault(&path)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 摄取一个 raw 文件：指纹检查 → 两步 LLM → Draft → 自动 Apply。
///
/// 两步端口按全局配置的双模型档位构造（fast 分析 / strong 成文）；
/// 引擎的 `on_progress` 回调在这里桥接为全局 `ingest-progress` 事件
/// （payload 即引擎的 `IngestStage`，`stage` 字段 snake_case）。
/// 模板取自 Vault 创建时绑定的 `.corpus/vault.json`。
#[tauri::command]
async fn ingest_file(
    app: AppHandle,
    store: State<'_, ConfigStore>,
    vault_path: String,
    raw_path: String,
) -> Result<IngestReport, String> {
    let config: AppConfig = store.get_config().map_err(|e| e.to_string())?;
    let fast = corpus_engine::build_llm_port(&config.fast).map_err(|e| e.to_string())?;
    let strong = corpus_engine::build_llm_port(&config.strong).map_err(|e| e.to_string())?;

    corpus_engine::ingest_file(
        &vault_path,
        &raw_path,
        &fast,
        &strong,
        |stage| {
            // 事件推送失败只影响进度展示，不阻断摄取
            let _ = app.emit(INGEST_PROGRESS_EVENT, &stage);
        },
    )
    .await
    .map_err(|e| e.to_string())
}
