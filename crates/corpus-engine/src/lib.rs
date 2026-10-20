//! Corpus 引擎：全部领域逻辑所在 crate（Vault、Template、Ingest、检索、Lint、图）。
//! 桌面、未来的 CLI 与 MCP 都只是它的薄壳。
//!
//! - T1 范围：Template 体系（编译期常量）、`create_vault` 公共 API、
//!   LLM Provider 测试接缝。
//! - T2 范围：全局配置层（[`config`]，sqlite，API key 只存环境变量名）、
//!   Vault 注册表（[`registry`]）、LLM 窄端口 [`LlmPort`] 与连通测试协议，
//!   以及 `rig` feature 之后的 Rig 真实实现（[`rig_llm`]）。
//! - T-UX 范围：[`vault::open_vault`]（打开已有 Vault 的只读校验）、
//!   [`wiki::list_pages`]（页面树纯文件系统遍历）、
//!   [`wiki::resolve_page_path`]（read_page 等只读入口的路径安全校验）
//!   与 [`wiki::read_page`]（页面读取唯一入口：Vault 校验 + 路径安全 +
//!   读原文三段全在引擎，桌面壳与 corpus-server 薄委托共用）。
//!   以及 [`import::import_raw`]（从任意位置复制进 raw/ 的唯一写入口，
//!   Ingest 流程「选文件 → 导入 → 摄取」的第一段）。
//! - T-W 范围：`ingest_file` 编排（import → 带上限端口 → ingest → 按
//!   `apply_mode` apply）从桌面命令层下沉为公共 API（[`ingest::IngestFileReport`]），
//!   桌面壳与 corpus-server 双壳共用，行为一致（票据 #17 v1）。

mod atomic;
mod git;

pub mod config;
pub mod engine;
pub mod error;
pub mod fingerprint;
pub mod import;
pub mod ingest;
pub mod llm;
pub mod page;
pub mod provider;
pub mod registry;
#[cfg(feature = "rig")]
pub mod rig_llm;
pub mod template;
pub mod vault;
pub mod wiki;

// 根 re-export 只保留外部消费者实际使用的名字（桌面壳与集成测试）；
// 其余类型经各自模块路径访问（如 `corpus_engine::template::GENERIC`），
// 仅引擎内部使用的类型不导出（如 ingest 的 LLM 解析结构）。
pub use config::{corpus_home, AppConfig, ApplyMode, ConfigStore, ModelTier};
pub use engine::Engine;
pub use error::{Error, Result};
pub use fingerprint::sha256_hex;
pub use import::{import_raw, ImportOutcome};
pub use ingest::{
    apply, ingest, ingest_file_with_ports, IngestDraft, IngestFileReport, IngestOutcome,
    IngestReport, IngestStage, IngestedPage, MANUAL_REVIEW_NOTE,
};
pub use llm::{
    ConnectionReport, LlmCallOutcome, LlmCallPurpose, LlmCallStats, LlmPort, LlmTier,
    ANALYSIS_MAX_TOKENS, COMPOSE_MAX_TOKENS,
};
pub use page::{parse_page, PageAction, PageKind};
pub use provider::Provider;
pub use registry::RecentVault;
#[cfg(feature = "rig")]
pub use rig_llm::{
    build_llm_port, build_llm_port_with_cap, build_model, test_connection, RigLlmPort,
};
// 完整编排（真实端口变体）属 rig 真实实现；端口注入变体
// `ingest_file_with_ports` 永远可用（见 ingest 模块文档）。
#[cfg(feature = "rig")]
pub use ingest::ingest_file;
pub use template::{
    builtin_template_infos, builtin_templates, template_by_id, TemplateId, TemplateInfo,
};
pub use vault::{create_vault, open_vault, vault_template_id, OpenedVault, VaultInfo};
pub use wiki::{list_pages, read_page, resolve_page_path, PageEntry, PageGroup, PageTree};
