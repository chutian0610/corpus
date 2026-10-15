// 桌面入口：薄壳，不改这里，逻辑在 lib.rs。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    corpus_desktop_lib::run()
}
