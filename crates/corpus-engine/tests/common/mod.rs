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
/// 察觉未预期的额外调用。只实现 `complete`：`complete_stream` 走 trait
/// 默认回退（整体结果一次 on_delta），即默认路径的测试形态。
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

    /// 弹出下一条脚本回复并记录调用。
    fn record_and_pop(&self, system: Option<&str>, user: &str) -> Result<String> {
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

#[async_trait::async_trait]
impl LlmPort for ScriptedLlmPort {
    async fn complete(&self, system: Option<&str>, user: &str) -> Result<String> {
        self.record_and_pop(system, user)
    }
}

/// 流式包装 [`ScriptedLlmPort`]：`complete_stream` 把脚本回复按
/// `chunk_chars` 字符分段、每段间隔 `chunk_delay_ms` 毫秒吐增量
///（模拟真模型的分段输出；0 = 不等待）。
///
/// 进度透明度测试接缝——驱动 ingest 管道的节流上报分支。
pub struct StreamingLlmPort {
    inner: ScriptedLlmPort,
    chunk_chars: usize,
    chunk_delay_ms: u64,
}

impl StreamingLlmPort {
    /// 包一个按序回放脚本的流式端口（调用记录语义同 ScriptedLlmPort）。
    pub fn over(replies: Vec<String>, chunk_chars: usize, chunk_delay_ms: u64) -> Self {
        Self {
            inner: ScriptedLlmPort::new(replies),
            chunk_chars,
            chunk_delay_ms,
        }
    }

    /// 已记录的调用数（透传内部 ScriptedLlmPort）。
    pub fn call_count(&self) -> usize {
        self.inner.call_count()
    }

    /// 已记录的调用快照（透传内部 ScriptedLlmPort）。
    pub fn recorded_calls(&self) -> Vec<(Option<String>, String)> {
        self.inner.recorded_calls()
    }
}

#[async_trait::async_trait]
impl LlmPort for StreamingLlmPort {
    async fn complete(&self, system: Option<&str>, user: &str) -> Result<String> {
        self.inner.complete(system, user).await
    }

    async fn complete_stream(
        &self,
        system: Option<&str>,
        user: &str,
        on_delta: &mut (dyn for<'s> FnMut(&'s str) + Send),
    ) -> Result<String> {
        let reply = self.inner.complete(system, user).await?;
        let chars: Vec<char> = reply.chars().collect();
        for piece in chars.chunks(self.chunk_chars) {
            if self.chunk_delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(self.chunk_delay_ms)).await;
            }
            on_delta(&piece.iter().collect::<String>());
        }
        Ok(reply)
    }
}

/// env 是进程全局态：涉及读写进程环境变量的测试（如 CORPUS_DEBUG）
/// 用本互斥锁串行化，避免并发测试互相污染。std Mutex：异步测试持锁跨
/// .await 是刻意的（摄取期间 env 必须保持设置），调用点显式 allow。
pub static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    // -c core.quotePath=false：Linux 上 git 默认对非 ASCII 文件名做八进制
    // 转义（"\350\207..."），会破坏 UTF-8 页面名断言——显式关闭，使断言
    // 与本机/CI 的全局 git 配置解耦（CI 首跑实测踩中）。
    let output = Command::new("git")
        .arg("-c")
        .arg("core.quotePath=false")
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
