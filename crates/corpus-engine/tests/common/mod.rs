//! 集成测试共享工具（Rust `tests/common/mod.rs` 惯例）。
//!
//! `mod.rs` 命名使其不会被测试 harness 当作独立测试二进制，仅供各
//! 集成测试通过 `mod common;` 引入；新增测试文件直接复用这里的
//! fake provider、临时 vault 路径与 git 断言工具，不各自复制。
//!
//! 各测试二进制只引用部分工具，dead_code 对共享模块是误报。
#![allow(dead_code)]

use std::collections::VecDeque;
use std::process::Command;
use std::sync::{Arc, Mutex};

use corpus_engine::{LlmPort, Provider, Result};

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

/// Scripted Fake [`LlmPort`]：按序回放脚本并记录每次 (system, user)。
///
/// T3 Ingest 管道的确定性测试接缝——两步（fast 分析 / strong 成文）各
/// 注入一个实例，断言调用次数与 prompt 内容；脚本耗尽报错以便测试
/// 察觉未预期的额外调用。
#[derive(Default)]
pub struct ScriptedLlmPort {
    /// 全部收到的调用（system, user）原样记录。
    pub calls: Mutex<Vec<(Option<String>, String)>>,
    replies: Mutex<VecDeque<String>>,
}

impl ScriptedLlmPort {
    /// 按给定回复序列构造（逐次弹出）。
    pub fn new(replies: Vec<String>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(replies.into()),
        }
    }

    /// 已记录的调用快照。
    pub fn recorded_calls(&self) -> Vec<(Option<String>, String)> {
        self.calls.lock().unwrap().clone()
    }

    /// 已记录的调用数。
    pub fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait::async_trait]
impl LlmPort for ScriptedLlmPort {
    async fn complete(&self, system: Option<&str>, user: &str) -> Result<String> {
        self.calls
            .lock()
            .unwrap()
            .push((system.map(str::to_string), user.to_string()));
        self.replies
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| corpus_engine::Error::Llm("Fake 脚本耗尽".to_string()))
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

/// Vault 全树快照（相对路径 → 内容哈希），用于「失败不产生半写」比对。
pub fn tree_snapshot(root: &std::path::Path, sub: &str) -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, prefix: &str, out: &mut Vec<(String, String)>) {
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .expect("read_dir 失败")
            .flatten()
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git" {
                continue; // git 内部状态不算工作区内容
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let path = entry.path();
            if path.is_dir() {
                walk(&path, &rel, out);
            } else {
                let content = std::fs::read(&path).expect("读文件失败");
                let sha = corpus_engine::sha256_hex(&content);
                out.push((rel, sha));
            }
        }
    }
    let mut out = Vec::new();
    walk(&root.join(sub), "", &mut out);
    out
}
