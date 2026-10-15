//! Corpus 引擎：全部领域逻辑所在 crate（Vault、Template、Ingest、检索、Lint、图）。
//! 桌面、未来的 CLI 与 MCP 都只是它的薄壳。
//!
//! - T1 范围：Template 体系（编译期常量）、`create_vault` 公共 API、
//!   LLM Provider 测试接缝。
//! - T2 范围：全局配置层（[`config`]，sqlite，API key 只存环境变量名）、
//!   Vault 注册表（[`registry`]）；LLM 窄端口与 Rig 真实实现随后续提交进入。

mod git;

pub mod config;
pub mod engine;
pub mod error;
pub mod provider;
pub mod registry;
pub mod template;
pub mod vault;

pub use config::{AppConfig, ApplyMode, ConfigStore, ModelTier, ProviderKind};
pub use engine::Engine;
pub use error::{Error, Result};
pub use provider::{NullProvider, Provider};
pub use registry::RecentVault;
pub use template::{
    builtin_template_infos, builtin_templates, template_by_id, PageTypeSpec, TemplateId,
    TemplateInfo, WikiTemplate, GENERIC, READING, RESEARCH,
};
pub use vault::{create_vault, VaultInfo};
