//! 引擎入口：持有注入的 LLM Provider，是全部领域操作的统一入口。

use std::path::Path;

use crate::error::Result;
use crate::provider::Provider;
use crate::template::TemplateId;
use crate::vault::{self, VaultInfo};

/// Corpus 引擎。
///
/// 构造时注入 [`Provider`]（trait object）；测试注入 fake provider，
/// T2 注入 Rig 实现的真实 Provider。T1 阶段引擎仅提供 `create_vault`，
/// 不触发任何 LLM 调用。
pub struct Engine {
    provider: Box<dyn Provider>,
}

impl Engine {
    /// 以注入的 Provider 构造引擎。
    pub fn new(provider: Box<dyn Provider>) -> Self {
        Self { provider }
    }

    /// 当前注入的 Provider。
    pub fn provider(&self) -> &dyn Provider {
        self.provider.as_ref()
    }

    /// 在 `path` 用 `template_id` 创建新 Vault，见 [`vault::create_vault`]。
    pub fn create_vault<P: AsRef<Path>>(
        &self,
        path: P,
        template_id: &TemplateId,
    ) -> Result<VaultInfo> {
        vault::create_vault(path, template_id)
    }
}
