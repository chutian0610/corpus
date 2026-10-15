//! 集成测试共享工具（Rust `tests/common/mod.rs` 惯例）。
//!
//! `mod.rs` 命名使其不会被测试 harness 当作独立测试二进制，仅供各
//! 集成测试通过 `mod common;` 引入；新增测试文件直接复用这里的
//! fake provider、临时 vault 路径与 git 断言工具，不各自复制。

use std::process::Command;
use std::sync::{Arc, Mutex};

use corpus_engine::{Provider, Result};

/// 记录全部 prompt 的 fake provider（scripted fake：原样回显）。
///
/// `prompts` 公开：各测试直接断言收到的 prompt 序列。
#[derive(Clone, Default)]
pub struct FakeProvider {
    pub prompts: Arc<Mutex<Vec<String>>>,
}

impl Provider for FakeProvider {
    fn complete(&self, prompt: &str) -> Result<String> {
        self.prompts.lock().unwrap().push(prompt.to_string());
        Ok(format!("fake: {prompt}"))
    }
}

/// 在临时目录下造一个尚不存在的 vault 根路径。
///
/// TempDir 与 root 一同返回：目录随句柄存活，测试期间不会被清理。
pub fn fresh_vault_path(tag: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let root = dir.path().join(tag);
    (dir, root)
}

/// 在 `repo` 内执行 git 并返回 stdout（测试断言用）。
pub fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("执行 git 失败");
    assert!(
        output.status.success(),
        "git {args:?} 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}
