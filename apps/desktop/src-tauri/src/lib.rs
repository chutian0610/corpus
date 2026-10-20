//! Corpus 桌面壳：全部领域逻辑在 corpus-engine，这里只做 Tauri 胶水。
//!
//! T2：setup 中解析 `app_data_dir()` 并注入 [`ConfigStore`]（引擎只认
//! 目录参数，见 T2 调研笔记第 6 节），新增配置 / 连通测试 / 最近 Vault
//! 命令。
//! T3：`ingest_file` 命令——引擎进度回调桥接为 `ingest-progress` 事件
//! （AppHandle: Emitter，见 T3 调研笔记第 4 节）。
//! T-UX（Ingest 流程修正）：`ingest_file` 两段化（import → 管道）；
//! T-UX 应用壳两级结构（启动页 + 工作区）的支撑命令——`open_vault`
//! （引擎校验 + 最近列表登记）、`list_pages`（页面树）、`read_page`
//! （预览用只读文本，校验与读取都在引擎唯一入口）；
//! `add_recent_vault` 退役（「打开」统一走 `open_vault` 的校验口径）。
//! T-W（票据 #17 v1）：`ingest_file` 编排（import → 端口带上限 →
//! ingest → 按 apply_mode apply）整体下沉为引擎公共 API
//! `corpus_engine::ingest_file`，本命令退化为薄委托——读全局配置 +
//! 桥接进度事件，与 corpus-server 双壳共用同一编排（行为一致）。

use corpus_engine::{
    AppConfig, ConfigStore, ConnectionReport, IngestFileReport, IngestStage, ModelTier,
    OpenedVault, PageTree, RecentVault, TemplateId, TemplateInfo, VaultInfo,
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
            // 不硬编码；CORPUS_HOME 环境变量优先（e2e / 沙盒隔离），
            // 未设置走平台默认。目录不存在则创建，随后打开全局 config.db。
            let dir = corpus_engine::corpus_home(app.path().app_data_dir()?);
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
            remove_recent_vault,
            open_vault,
            list_pages,
            read_page,
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

/// 从最近列表移除一个 Vault（只动注册表，不删文件）。
#[tauri::command]
async fn remove_recent_vault(store: State<'_, ConfigStore>, path: String) -> Result<(), String> {
    store
        .remove_recent_vault(&path)
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// 打开一个已有 Vault（T-UX 启动页 / 工作区的核心入口）。
///
/// 引擎 `open_vault` 做只读校验（`.corpus/vault.json` 存在且模板 id 可
/// 解析），通过后由本命令登记最近列表（全局配置层职责：引擎不越权，
/// 壳层攒齐两份数据再写）。登记失败不阻断打开——Vault 已真实有效，
/// 只是「最近」少了这条记录，打日志即可。
#[tauri::command]
async fn open_vault(store: State<'_, ConfigStore>, path: String) -> Result<OpenedVault, String> {
    let opened = corpus_engine::open_vault(&path).map_err(|e| e.to_string())?;
    if let Err(e) = store.add_recent_vault(&path, &opened.template_id) {
        eprintln!("写入最近 Vault 注册表失败（不影响打开结果）：{e}");
    }
    Ok(opened)
}

/// 列出当前 Vault 的 wiki/ 页面树（文件面板数据源）。
///
/// 纯文件系统遍历（引擎零 LLM 零写入），路径不合法 / 非 Vault 由引擎
/// 报中文错误。
#[tauri::command]
async fn list_pages(vault_path: String) -> Result<PageTree, String> {
    corpus_engine::list_pages(&vault_path).map_err(|e| e.to_string())
}

/// 只读读取 `wiki/` 下某页面的原始文本（预览面板；正式渲染属 T4）。
///
/// 全部校验与读取都在引擎 [`corpus_engine::read_page`] 单一入口（open_vault
/// 级 Vault 校验 + resolve_page_path 路径安全关口 + 读原文三段：拒绝绝对
/// 路径 / `..` / 非 `.md` / symlink 逃逸）——本命令只做委托与错误字符串化，
/// 与 corpus-server 双壳同一口径，不重复胶水。
#[tauri::command]
async fn read_page(vault_path: String, rel_path: String) -> Result<String, String> {
    corpus_engine::read_page(&vault_path, &rel_path).map_err(|e| e.to_string())
}

/// 摄取一个文档（薄委托）：编排（`import_raw` 导入 → 双档位端口带上限
/// → `ingest` 管道 → 按 `apply_mode` 分支 apply）已整体下沉为引擎公共
/// API [`corpus_engine::ingest_file`]（票据 #17 v1，桌面壳与
/// corpus-server 共用同一编排，行为一致）。本命令只负责壳层职责：
/// 读全局配置（apply_mode / 双模型档位）与把引擎进度回调桥接为全局
/// `ingest-progress` 事件（payload 即引擎 `IngestStage`；分析/生成两步
/// 的 LLM 流式增量 chars 原样 emit）。
///
/// 响应为引擎 [`IngestFileReport`]（serde flatten 平铺 + applied/note/
/// imported_raw_path/newly_imported/summary_lines），字段形状与前端
/// `IngestReport` 接口一致。
#[tauri::command]
async fn ingest_file(
    app: AppHandle,
    store: State<'_, ConfigStore>,
    vault_path: String,
    raw_path: String,
) -> Result<IngestFileReport, String> {
    let config = store.get_config().map_err(|e| e.to_string())?;
    let emit_progress = move |stage: IngestStage| {
        // 事件推送失败只影响进度展示，不阻断摄取
        let _ = app.emit(INGEST_PROGRESS_EVENT, &stage);
    };
    corpus_engine::ingest_file(&vault_path, &raw_path, &config, emit_progress)
        .await
        .map_err(|e| e.to_string())
}
