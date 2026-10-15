//! Corpus 引擎：全部领域逻辑所在 crate（Vault、Template、Ingest、检索、Lint、图）。
//! 桌面、未来的 CLI 与 MCP 都只是它的薄壳。
//!
//! T1 范围：Template 体系（编译期常量）、`create_vault` 公共 API、
//! LLM Provider 测试接缝。

mod git;

pub mod engine;
pub mod error;
pub mod provider;
pub mod template;
pub mod vault;

pub use engine::Engine;
pub use error::{Error, Result};
pub use provider::{NullProvider, Provider};
pub use template::{
    builtin_template_infos, builtin_templates, template_by_id, PageTypeSpec, TemplateId,
    TemplateInfo, WikiTemplate, GENERIC, READING, RESEARCH,
};
pub use vault::{create_vault, VaultInfo};
