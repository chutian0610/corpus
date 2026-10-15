//! 引擎统一错误类型（thiserror 结构化）。

use std::path::PathBuf;

/// Corpus 引擎错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// create_vault 的目标路径已存在：拒绝覆盖，保持原样。
    #[error("目标路径已存在，拒绝覆盖：{path}")]
    VaultPathExists {
        /// 已存在的路径。
        path: PathBuf,
    },

    /// 给定的模板 id 不在内置模板中。
    #[error("未知模板 id：{id}（可用模板：{available}）")]
    UnknownTemplate {
        /// 传入的模板 id。
        id: String,
        /// 可用模板 id 列表（逗号分隔）。
        available: String,
    },

    /// 文件系统 IO 错误。
    #[error("IO 错误：{0}")]
    Io(#[from] std::io::Error),

    /// shell out 到系统 git 失败（无法启动或非零退出）。
    #[error("git 命令失败（git {command}）：{stderr}")]
    Git {
        /// 失败的 git 子命令（不含可执行名）。
        command: String,
        /// git 的 stderr 输出。
        stderr: String,
    },

    /// LLM Provider 不可用（如未注入真实实现即被调用）。
    #[error("LLM Provider 不可用：{0}")]
    ProviderUnavailable(String),
}

/// 引擎统一的 `Result` 别名。
pub type Result<T> = std::result::Result<T, Error>;
