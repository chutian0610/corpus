//! Corpus 桌面壳：全部领域逻辑在 corpus-engine，这里只做 Tauri 胶水。
//!
//! T2：setup 中解析 `app_data_dir()` 并注入 [`ConfigStore`]（引擎只认
//! 目录参数，见 T2 调研笔记第 6 节），新增配置 / 连通测试 / 最近 Vault
//! 命令。
//! T3：`ingest_file` 命令——引擎进度回调桥接为 `ingest-progress` 事件
//! （AppHandle: Emitter，见 T3 调研笔记第 4 节），并按全局配置
//! `apply_mode` 分支：`auto` 显式 apply，`manual` 不落盘返回 Draft 检视
//! 清单（完整人审流程属票据 T5）。
//! T-UX（Ingest 流程修正）：`ingest_file` 改为两段——先经引擎
//! `import_raw` 把任意位置的源文件导入（复制）进 raw/，再走既有管道
//! （管道只接受 raw/ 内路径的契约不变）；用户不再被要求先手动把
//! 文件拷进 raw/。
//! T-UX：应用壳两级结构（启动页 + 工作区）的支撑命令——`open_vault`
//! （引擎校验 + 最近列表登记）、`list_pages`（页面树）、`read_page`
//! （预览用只读文本，路径安全由引擎 `resolve_page_path` 校验）；
//! `add_recent_vault` 退役（「打开」统一走 `open_vault` 的校验口径，
//! 不再允许无校验登记）。

use corpus_engine::{
    AppConfig, ApplyMode, ConfigStore, ConnectionReport, IngestOutcome, IngestReport, IngestStage,
    ModelTier, OpenedVault, PageTree, RecentVault, TemplateId, TemplateInfo, VaultInfo,
};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

/// 前端 listen 的 Ingest 进度事件名。
pub const INGEST_PROGRESS_EVENT: &str = "ingest-progress";

/// manual 模式（apply_mode=manual）结果的注明：Draft 未落盘，完整的
/// 人审流程（检视 / 编辑 / 放行）属票据 T5。
const MANUAL_REVIEW_NOTE: &str = "人审模式：Draft 未落盘，完整人审流程属 T5";

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
/// 路径安全全部由引擎 [`corpus_engine::resolve_page_path`] 单一关口承担
/// （拒绝绝对路径 / `..` / 非 `.md`，canonicalize 断言不逃出 Vault）；
/// 读取前先对 `vault_path` 跑 `open_vault` 级校验（`.corpus/vault.json`
/// 存在且模板可解析），与「打开」同一口径。
#[tauri::command]
async fn read_page(vault_path: String, rel_path: String) -> Result<String, String> {
    corpus_engine::open_vault(&vault_path).map_err(|e| e.to_string())?;
    let full =
        corpus_engine::resolve_page_path(&vault_path, &rel_path).map_err(|e| e.to_string())?;
    std::fs::read_to_string(&full).map_err(|e| format!("读取 {} 失败：{e}", full.display()))
}

/// ingest_file 命令响应：引擎 [`IngestReport`] 平铺 + 导入信息 + Apply 模式语义。
#[derive(Debug, Clone, Serialize)]
struct IngestFileReport {
    #[serde(flatten)]
    report: IngestReport,
    /// 导入后的 raw 文件相对 Vault 路径（如 `raw/foo.md`，引擎
    /// `import_raw` 的结果；管道实际摄取的就是它）。
    imported_raw_path: String,
    /// 本次是否新导入进 raw/（false = 已在库 / 同内容去重）。
    newly_imported: bool,
    /// Draft 是否已落盘（指纹跳过与 manual 模式为 false）。
    applied: bool,
    /// manual 模式：Draft 人读检视清单（每页一行，降级页带标注）。
    summary_lines: Vec<String>,
    /// manual 模式注明（见 [`MANUAL_REVIEW_NOTE`]）。
    note: Option<String>,
}

/// 摄取一个文档（两段）：先 `import_raw` 把任意位置的源文件导入
/// （复制）进 Vault 的 raw/（源文件不动；同名去重 / 后缀递增），再走
/// 既有 Ingest 管道：指纹检查 → 两步 LLM → Draft，按全局配置
/// `apply_mode` 决定是否落盘（CONTEXT.md「Apply」）。管道契约不变——
/// 只接受 raw/ 内路径，入口由本命令的导入段保证。
///
/// 两步端口按全局配置的双模型档位构造（fast 分析 / strong 成文）；
/// 引擎的 `on_progress` 回调在这里桥接为全局 `ingest-progress` 事件
/// （payload 即引擎的 `IngestStage`，`stage` 字段 snake_case）。
/// 模板取自 Vault 创建时绑定的 `.corpus/vault.json`。
/// - `auto`：显式 apply（原子写 + 恰好一个 git 提交 + 指纹记录）；
/// - `manual`：不落盘不提交，返回 Draft 的 summary_lines 并注明
///   「人审模式：Draft 未落盘，完整人审流程属 T5」。
#[tauri::command]
async fn ingest_file(
    app: AppHandle,
    store: State<'_, ConfigStore>,
    vault_path: String,
    raw_path: String,
) -> Result<IngestFileReport, String> {
    // 第一段：导入（任意位置 → raw/；已在库则原样通过）。
    let imported = corpus_engine::import_raw(&vault_path, &raw_path).map_err(|e| e.to_string())?;

    let config: AppConfig = store.get_config().map_err(|e| e.to_string())?;
    let fast = corpus_engine::build_llm_port(&config.fast).map_err(|e| e.to_string())?;
    let strong = corpus_engine::build_llm_port(&config.strong).map_err(|e| e.to_string())?;

    let emit_progress = |stage: IngestStage| {
        // 事件推送失败只影响进度展示，不阻断摄取
        let _ = app.emit(INGEST_PROGRESS_EVENT, &stage);
    };

    // 第二段：既有 Ingest 管道（只接受 raw/ 内路径）。
    match corpus_engine::ingest(
        &vault_path,
        &imported.raw_abs,
        &fast,
        &strong,
        emit_progress,
    )
    .await
    .map_err(|e| e.to_string())?
    {
        IngestOutcome::Skipped(report) => Ok(IngestFileReport {
            report,
            imported_raw_path: imported.raw_rel,
            newly_imported: imported.imported,
            applied: false,
            summary_lines: Vec::new(),
            note: None,
        }),
        IngestOutcome::Draft(draft) => match config.apply_mode {
            ApplyMode::Auto => {
                let commit = corpus_engine::apply(&vault_path, &draft, emit_progress)
                    .map_err(|e| e.to_string())?;
                Ok(IngestFileReport {
                    report: draft.report(Some(commit)),
                    imported_raw_path: imported.raw_rel,
                    newly_imported: imported.imported,
                    applied: true,
                    summary_lines: Vec::new(),
                    note: None,
                })
            }
            // 人审模式：Draft 不落盘（零写入零提交），交还检视清单
            ApplyMode::Manual => Ok(IngestFileReport {
                report: draft.report(None),
                imported_raw_path: imported.raw_rel,
                newly_imported: imported.imported,
                applied: false,
                summary_lines: draft.summary_lines(),
                note: Some(MANUAL_REVIEW_NOTE.to_string()),
            }),
        },
    }
}
