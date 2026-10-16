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

    /// 路径不是本引擎管理的有效 Vault（缺骨架 / 缺元数据）。
    #[error("路径不是有效的 Corpus Vault：{}（{reason}）", path.display())]
    NotAVault {
        /// 被拒绝的 Vault 根路径。
        path: PathBuf,
        /// 具体原因（缺哪个文件 / 什么校验失败）。
        reason: String,
    },

    /// create_vault 的目标目录已经是一个 Corpus Vault（存在 .corpus/vault.json）。
    ///
    /// 与 [`Error::VaultPathExists`]（泛泛的「非空拒绝」）区分：此错误明确
    /// 引导用户走「打开」而非重复创建（应用壳启动页据此提示）。
    #[error("该目录已是一个 Vault，请从启动页打开：{path}")]
    AlreadyAVault {
        /// 已是 Vault 的目标路径。
        path: PathBuf,
    },

    /// 页面相对路径非法（read_page 等只读入口的路径安全校验）。
    #[error("非法页面路径：{path}（{reason}）")]
    InvalidPagePath {
        /// 被拒绝的相对路径原样。
        path: String,
        /// 具体原因（绝对路径 / `..` 逃逸 / 非 `.md` / 解析逃出 Vault 等）。
        reason: String,
    },

    /// Ingest 的 raw 文件不在 Vault 的 raw/ 目录内。
    #[error("raw 文件必须在 Vault 的 raw/ 目录内：{}（Vault：{}）", raw.display(), vault.display())]
    RawNotInVault {
        /// 传入的 raw 文件路径。
        raw: PathBuf,
        /// Vault 根路径。
        vault: PathBuf,
    },

    /// LLM 返回的输出无法解析为引擎期望的结构（重试耗尽后）。
    #[error("LLM 输出无法解析：{0}")]
    BadLlmOutput(String),

    /// YAML frontmatter 序列化/反序列化失败。
    #[error("YAML frontmatter 错误：{0}")]
    Yaml(#[from] serde_yaml_ng::Error),

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
