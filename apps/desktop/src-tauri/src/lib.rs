//! Corpus 桌面壳：全部领域逻辑在 corpus-engine，这里只做 Tauri 胶水。

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
