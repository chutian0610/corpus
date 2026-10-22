//! Corpus 引擎：全部领域逻辑所在 crate（Vault、Template、Ingest、检索、Lint、图）。
//! 桌面、未来的 CLI 与 MCP 都只是它的薄壳。
//!
//! - T1 范围：Template 体系（编译期常量）、`create_vault` 公共 API、
//!   LLM Provider 测试接缝。
//! - T2 范围：全局配置层（[`config`]，sqlite，API key 只存环境变量名）、
//!   Vault 注册表（[`registry`]）、LLM 窄端口 [`LlmPort`] 与连通测试协议，
//!   以及 `rig` feature 之后的 Rig 真实实现（[`rig_llm`]）。

mod atomic;
mod git;

pub mod config;
pub mod engine;
pub mod error;
pub mod fingerprint;
pub mod ingest;
pub mod llm;
pub mod page;
pub mod provider;
pub mod registry;
#[cfg(feature = "rig")]
pub mod rig_llm;
pub mod template;
pub mod vault;

// 根 re-export 只保留外部消费者实际使用的名字（桌面壳与集成测试）；
// 其余类型经各自模块路径访问（如 `corpus_engine::template::GENERIC`），
// 仅引擎内部使用的类型不导出（如 ingest 的 LLM 解析结构）。
pub use config::{AppConfig, ApplyMode, ConfigStore, ModelTier};
pub use engine::Engine;
pub use error::{Error, Result};
pub use fingerprint::sha256_hex;
pub use ingest::{
    apply, ingest, IngestDraft, IngestOutcome, IngestReport, IngestStage, IngestedPage,
};
pub use llm::{ConnectionReport, LlmPort};
pub use page::{parse_page, PageAction, PageKind};
pub use provider::Provider;
pub use registry::RecentVault;
#[cfg(feature = "rig")]
pub use rig_llm::{build_llm_port, build_model, test_connection, RigLlmPort};
pub use template::{
    builtin_template_infos, builtin_templates, template_by_id, TemplateId, TemplateInfo,
};
pub use vault::{create_vault, vault_template_id, VaultInfo};
