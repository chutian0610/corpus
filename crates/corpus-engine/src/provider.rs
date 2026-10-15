//! LLM Provider 抽象：T1 的最小测试接缝。
//!
//! 引擎所有需要 LLM 的管线（Ingest 两步、查询综合、矛盾检测）都经由
//! [`Provider`] trait 调用；真实实现由外部注入，测试注入 fake provider
//! 保证确定性、离线。T2 将引入 Rig 提供真实实现。

use crate::error::{Error, Result};

/// LLM Provider：一次文本补全的最小抽象。
pub trait Provider: Send + Sync {
    /// 用给定 prompt 请求一次补全，返回模型输出文本。
    fn complete(&self, prompt: &str) -> Result<String>;
}

/// 占位 Provider：`create_vault` 等不依赖 LLM 的操作用它构造引擎。
///
/// 一旦被调用即返回 [`Error::ProviderUnavailable`]——T1 阶段任何代码路径
/// 都不应触发 LLM 调用，真实实现（Rig）在 T2 注入。
#[derive(Debug, Default, Clone, Copy)]
pub struct NullProvider;

impl Provider for NullProvider {
    fn complete(&self, _prompt: &str) -> Result<String> {
        Err(Error::ProviderUnavailable(
            "尚未注入真实 LLM Provider（T2 引入 Rig 实现）".to_string(),
        ))
    }
}
