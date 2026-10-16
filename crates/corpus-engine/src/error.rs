//! 引擎统一错误类型（thiserror 结构化）。

use std::path::PathBuf;

use crate::template::TemplateId;

/// Corpus 引擎错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// create_vault 的目标路径已存在且非空：拒绝覆盖，保持原样。
    #[error("目标路径已存在且非空，拒绝覆盖：{path}")]
    VaultPathExists {
        /// 已存在的路径。
        path: PathBuf,
    },

    /// 给定的模板 id 不在内置模板中。
    #[error("未知模板 id：{id}（可用模板：{available}）")]
    UnknownTemplate {
        /// 传入的模板 id。
        id: TemplateId,
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

    /// sqlite 操作失败（config.db）。
    #[error("sqlite 错误：{0}")]
    Db(#[from] rusqlite::Error),

    /// 配置的 JSON 序列化/反序列化失败。
    #[error("配置 JSON 错误：{0}")]
    Json(#[from] serde_json::Error),

    /// 配置内容无效（如模型名未填、兼容端点缺 base_url）。
    #[error("配置无效：{0}")]
    InvalidConfig(String),

    /// API key 对应的环境变量未设置。
    ///
    /// API key 的值只从环境变量读取，永不落盘；此错误意味着调用方
    /// 需要先在运行环境中设置该变量。
    #[error("环境变量 {env} 未设置：API key 只从环境变量读取，值不会写入任何文件")]
    MissingApiKey {
        /// 期望读取的环境变量名。
        env: String,
    },

    /// LLM 调用失败（网络、鉴权、模型返回错误等）。
    #[error("LLM 调用失败：{0}")]
    Llm(String),
}

/// 引擎统一的 `Result` 别名。
pub type Result<T> = std::result::Result<T, Error>;
