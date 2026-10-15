//! Corpus 桌面壳：全部领域逻辑在 corpus-engine，这里只做 Tauri 胶水。

use corpus_engine::{TemplateId, TemplateInfo, VaultInfo};

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![create_vault, list_templates])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// 创建 Vault：委托 corpus-engine 的 create_vault 公共 API。
///
/// async + owned 参数（见调研笔记坑清单：引用参数需 Result、主线程不阻塞）；
/// 引擎错误映射为字符串供前端展示（T2 引入结构化错误序列化）。
#[tauri::command]
async fn create_vault(path: String, template_id: TemplateId) -> Result<VaultInfo, String> {
    corpus_engine::create_vault(&path, &template_id).map_err(|e| e.to_string())
}

/// 列出内置模板元数据：前端模板选择器的单一数据源。
///
/// 模板清单与 id 集合由引擎 `builtin_templates` 下发，壳层不硬编码；
/// `TemplateId` serde 透明，前端 JSON 中仍是字符串。
#[tauri::command]
async fn list_templates() -> Vec<TemplateInfo> {
    corpus_engine::builtin_template_infos()
}
