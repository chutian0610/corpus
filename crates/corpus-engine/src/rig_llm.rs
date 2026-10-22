//! [`LlmPort`] 的 Rig 实现：provider 工厂 + 真实「测试连通」。
//!
//! 全部在 `rig` feature 之后（默认开启；引擎可在无 rig feature 下编译
//! 测试，见根 Cargo.toml）。rig 0.44 API 依据 T2 调研笔记第 2、3 节：
//! - OpenAI 兼容端点走 `OpenAIConfig`，强制 `Route::Chat` 并启用
//!   `with_system_instructions_as_messages()` 兜底（preamble 改走 input
//!   里的 system 消息，兼容拒绝顶层 `instructions` 字段的后端）；
//! - system prompt 经 `AgentBuilder::preamble()` 注入；
//! - DynModel 是 `Clone`（Arc 内部），端口持有一次构造的模型即可。

use futures_util::StreamExt;
use rig::agent::MultiTurnStreamItem;
use rig::operation::Completion;
use rig::providers::anthropic::AnthropicConfig;
use rig::providers::openai::{OpenAIConfig, Route};
use rig::streaming::{Item, StreamEvent};
use rig::{AgentBuilder, DynModel};

use crate::config::{ModelTier, ProviderKind};
use crate::error::{Error, Result};
use crate::llm::{test_connection_with, ConnectionReport, LlmPort};

/// LlmPort 的 Rig 实现：持有构造好的 `DynModel<Completion>` 与可选的
/// 服务端输出上限（token）、透传参数。
///
/// `max_tokens` 与 `additional_params` 都是 `AgentBuilder` 的**构建期**
/// 参数（rig 0.44），而 `complete` / `complete_stream` 协议无法按调用
/// 携带用途——因此在端口构造时预设：Ingest 两条路径与档位一一对应
/// （fast 只用于分析、strong 只用于成文），按档位构造端口即等价于按
/// 用途配置（取舍见 [`build_llm_port_with_cap`]）。`max_tokens` 是服务端
/// 硬顶（第一道闸），引擎侧的字符熔断是第二道闸（见 [`crate::ingest`]）；
/// `additional_params` 原样并入请求体顶层（provider passthrough），
/// 用于 MiniMax M3.1 等推理模型的 `reasoning_effort` / `reasoning_split`
/// 等透传参数（见 [`crate::ingest::analysis_passthrough_params`]）。
pub struct RigLlmPort {
    model: DynModel<Completion>,
    max_tokens: Option<u64>,
    additional_params: Option<serde_json::Value>,
}

impl RigLlmPort {
    /// 按本次调用的 system（可选）构造 AgentBuilder，应用端口预设的
    /// preamble、max_tokens 与透传参数（两处构建共用）。
    fn agent_builder(&self, system: Option<&str>) -> AgentBuilder {
        let mut builder = AgentBuilder::new(self.model.clone());
        if let Some(system) = system {
            builder = builder.preamble(system);
        }
        if let Some(max_tokens) = self.max_tokens {
            builder = builder.max_tokens(max_tokens);
        }
        if let Some(params) = &self.additional_params {
            builder = builder.additional_params(params.clone());
        }
        builder
    }
}

#[async_trait::async_trait]
impl LlmPort for RigLlmPort {
    async fn complete(&self, system: Option<&str>, user: &str) -> Result<String> {
        let agent = self.agent_builder(system).build();
        let response = agent
            .prompt(user.to_string())
            .max_turns(1)
            .await
            .map_err(|e| Error::Llm(e.to_string()))?;
        Ok(response.output())
    }

    /// 真流式：`agent.prompt(..).stream()` 循环消费 [`MultiTurnStreamItem`]，
    /// 文本增量一到达就回调 `on_delta`；终值以 `FinalResponse` 的聚合文本
    /// 为准（`PromptResponse::output()` 与非流式 [`LlmPort::complete`]
    /// 同源，返回语义一致）。流意外提前结束时以已聚合增量兜底。
    async fn complete_stream(
        &self,
        system: Option<&str>,
        user: &str,
        on_delta: &mut (dyn for<'s> FnMut(&'s str) + Send),
    ) -> Result<String> {
        let agent = self.agent_builder(system).build();
        let mut stream = agent.prompt(user.to_string()).max_turns(1).stream();
        let mut aggregated = String::new();
        while let Some(item) = stream.next().await {
            match item.map_err(|e| Error::Llm(e.to_string()))? {
                MultiTurnStreamItem::StreamAssistantItem(Item::Event(StreamEvent::Text {
                    text,
                    ..
                })) => {
                    aggregated.push_str(&text);
                    on_delta(&text);
                }
                MultiTurnStreamItem::FinalResponse(response) => {
                    return Ok(response.output());
                }
                _ => {}
            }
        }
        if aggregated.is_empty() {
            Err(Error::Llm(
                "流式响应提前结束（未产生任何文本与最终结果）".to_string(),
            ))
        } else {
            // 无 FinalResponse 的异常结束：增量聚合兜底，不丢已到手的输出
            Ok(aggregated)
        }
    }
}

/// OpenAI 型（openai / openai_compatible）配置构造。
///
/// 独立成公共函数是为了让测试能直接断言配置对象上的
/// `base_url` / 路由 / system prompt 兜底，而不必发起网络请求。
pub fn openai_config(tier: &ModelTier, api_key: &str) -> Result<OpenAIConfig> {
    match tier.provider {
        ProviderKind::OpenAICompatible => {
            let base_url = tier.base_url.as_deref().ok_or_else(|| {
                Error::InvalidConfig("OpenAI 兼容端点必须配置 base_url".to_string())
            })?;
            // 兼容端点双兜底：Chat Completions 是唯一全方言保证的端点；
            // system-instructions-as-messages 防后端忽略/拒绝顶层 instructions。
            Ok(OpenAIConfig::new(api_key)
                .with_base_url(base_url)
                .with_route(Route::Chat)
                .with_system_instructions_as_messages())
        }
        ProviderKind::OpenAI => {
            let mut config = OpenAIConfig::new(api_key);
            if let Some(base_url) = tier.base_url.as_deref() {
                config = config.with_base_url(base_url);
            }
            Ok(config)
        }
        other => Err(Error::InvalidConfig(format!(
            "openai_config 不适用于 provider：{}",
            other.as_str()
        ))),
    }
}

/// Anthropic 配置构造（公共供测试断言 base_url 归一化，不发网络）。
pub fn anthropic_config(tier: &ModelTier, api_key: &str) -> Result<AnthropicConfig> {
    if tier.provider != ProviderKind::Anthropic {
        return Err(Error::InvalidConfig(format!(
            "anthropic_config 不适用于 provider：{}",
            tier.provider.as_str()
        )));
    }
    let mut config = AnthropicConfig::new(api_key);
    if let Some(base_url) = tier.base_url.as_deref() {
        config = config.with_base_url(base_url);
    }
    Ok(config)
}

/// 按档位构造 DynModel（provider 工厂；不发起网络请求）。
///
/// 更换供应商只改配置：工厂依据 [`ProviderKind`] 分派——
/// - openai：`OpenAIConfig::client().completion(model)`（官方端点默认 Responses 线路）；
/// - openai_compatible：`Route::Chat` + system prompt 兜底（见 [`openai_config`]）；
/// - anthropic：`AnthropicConfig::client().completion(model)`；
/// - deepseek：`deepseek::new` 预配置方言与 base_url，走 `.chat(model)`。
pub fn build_model(tier: &ModelTier, api_key: &str) -> Result<DynModel<Completion>> {
    if tier.model.trim().is_empty() {
        return Err(Error::InvalidConfig(format!(
            "模型名未配置（provider：{}）",
            tier.provider.as_str()
        )));
    }
    match tier.provider {
        ProviderKind::OpenAI | ProviderKind::OpenAICompatible => Ok(openai_config(tier, api_key)?
            .client()
            .completion(tier.model.clone())
            .into()),
        // MiniMax 原生方言（OpenAI 格式线）：默认国内站 api.minimaxi.com，
        // 国际站经 base_url 覆盖为 https://api.minimax.io/v1。模型常量表
        // 尚无 M3.x（rig 未跟上），模型名直接用配置字符串。
        ProviderKind::Minimax => {
            let mut config =
                OpenAIConfig::with_key(&rig::providers::openai::wire::MINIMAX_CHINA, api_key);
            if let Some(base_url) = &tier.base_url {
                config = config.with_base_url(base_url.clone());
            }
            Ok(config.client().chat(tier.model.clone()).into())
        }
        ProviderKind::Anthropic => Ok(anthropic_config(tier, api_key)?
            .client()
            .completion(tier.model.clone())
            .into()),
        ProviderKind::DeepSeek => {
            if tier.base_url.is_some() {
                return Err(Error::InvalidConfig(
                    "DeepSeek 端点固定，不支持自定义 base_url；自建网关请改用 openai_compatible"
                        .to_string(),
                ));
            }
            Ok(rig::providers::deepseek::new(api_key)
                .chat(tier.model.clone())
                .into())
        }
    }
}

/// 按档位构造端口：校验配置 → 解析 key（配置明文字段，ADR-0003）→ 工厂建模型。
///
/// 不设输出上限（连通测试等最小调用用这个；上限对只回一个词的探针
/// 无意义）。
pub fn build_llm_port(tier: &ModelTier) -> Result<RigLlmPort> {
    build_llm_port_with_cap(tier, None)
}

/// 按档位构造端口并预设服务端输出上限（token，`AgentBuilder::max_tokens`，
/// rig 0.44 构建期参数）。
///
/// 取舍：`complete` / `complete_stream` 协议无法按调用携带用途参数，
/// `max_tokens` 只能在端口构造时定死；Ingest 的两条路径与档位一一对应
/// （fast 档只用于分析、strong 档只用于成文），因此调用方按档位传上限
/// 即等价于按用途设限——桌面壳对 fast 档传 [`crate::llm::ANALYSIS_MAX_TOKENS`]、
/// strong 档传 [`crate::llm::COMPOSE_MAX_TOKENS`]。这是服务端硬顶
/// （第一道闸）；引擎侧还有按字符数的熔断（第二道闸，见 [`crate::ingest`]），
/// 两道闸独立生效。
pub fn build_llm_port_with_cap(tier: &ModelTier, max_tokens: Option<u64>) -> Result<RigLlmPort> {
    build_llm_port_with(tier, max_tokens, None)
}

/// [`build_llm_port_with_cap`] 的完整形态：额外携带透传参数
/// （`AgentBuilder::additional_params`，原样并入请求体顶层）。
///
/// 典型用途：MiniMax M3.1 系等推理模型强制思考且思考 token 计入
/// `max_tokens` 预算，分析调用传 `reasoning_effort` / `reasoning_split`
/// 压低思考量并分离思考与正文（参数事实与取舍见
/// [`crate::ingest::analysis_passthrough_params`]；`thinking` 开关不可用
/// ——M3.1 上 `thinking.type=disabled` 返回 400，只有旧 M3 可关）。
pub fn build_llm_port_with(
    tier: &ModelTier,
    max_tokens: Option<u64>,
    additional_params: Option<serde_json::Value>,
) -> Result<RigLlmPort> {
    let api_key = tier.resolve_api_key()?;
    Ok(RigLlmPort {
        model: build_model(tier, &api_key)?,
        max_tokens,
        additional_params,
    })
}

/// 真实「测试连通」：按 `tier` 构造端口并发起一次最小补全调用。
///
/// 本地可判定的失败（模型名未配置、未配置 API key、兼容端点缺
/// base_url）以 [`Error::InvalidConfig`] / [`Error::MissingApiKey`] 报告；
/// 远端失败以 [`Error::Llm`] 报告。均不发任何回退网络请求。
pub async fn test_connection(tier: &ModelTier) -> Result<ConnectionReport> {
    test_connection_with(&build_llm_port(tier)?).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ANALYSIS_MAX_TOKENS;
    use rig::providers::openai::responses_api::SystemInstructionsPlacement;

    fn tier(provider: ProviderKind, model: &str, base_url: Option<&str>) -> ModelTier {
        ModelTier {
            provider,
            model: model.to_string(),
            base_url: base_url.map(str::to_string),
            api_key: None,
        }
    }

    /// 票据验收：provider 工厂对 OpenAI 兼容端点启用 system prompt 兜底。
    #[test]
    fn openai_compatible_config_enables_system_prompt_fallback_and_chat_route() {
        let config = openai_config(
            &tier(
                ProviderKind::OpenAICompatible,
                "some-model",
                Some("https://gw.example.com/v1"),
            ),
            "key",
        )
        .expect("构造兼容端点配置失败");
        assert_eq!(config.base_url, "https://gw.example.com/v1");
        assert_eq!(config.completion_route(), Route::Chat);
        assert_eq!(
            config.system_instructions_placement(),
            SystemInstructionsPlacement::InputSystemMessages,
            "兼容端点必须把 preamble 改走 input 里的 system 消息"
        );
    }

    /// 官方 OpenAI 不动默认策略（Responses 线路 + 顶层 instructions），
    /// 只显式配置了 base_url 时才覆盖。
    #[test]
    fn plain_openai_keeps_dialect_defaults() {
        let config = openai_config(&tier(ProviderKind::OpenAI, "gpt-5.2", None), "key")
            .expect("构造 OpenAI 配置失败");
        assert_eq!(config.base_url, "https://api.openai.com/v1");
        assert_eq!(config.completion_route(), Route::Responses);
        assert_eq!(
            config.system_instructions_placement(),
            SystemInstructionsPlacement::Instructions
        );

        let overridden = openai_config(
            &tier(
                ProviderKind::OpenAI,
                "gpt-5.2",
                Some("https://proxy.internal/v1"),
            ),
            "key",
        )
        .expect("构造自定义 base_url 配置失败");
        assert_eq!(overridden.base_url, "https://proxy.internal/v1");
    }

    #[test]
    fn openai_compatible_requires_base_url() {
        let err = openai_config(&tier(ProviderKind::OpenAICompatible, "m", None), "key")
            .expect_err("缺 base_url 应报错");
        assert!(matches!(err, Error::InvalidConfig(ref msg) if msg.contains("base_url")));
    }

    #[test]
    fn anthropic_config_applies_and_normalizes_base_url() {
        let config = anthropic_config(
            &tier(ProviderKind::Anthropic, "claude-sonnet-4-6", None),
            "key",
        )
        .expect("构造 Anthropic 配置失败");
        assert_eq!(config.base_url, "https://api.anthropic.com");

        let custom = anthropic_config(
            &tier(
                ProviderKind::Anthropic,
                "claude-sonnet-4-6",
                Some("https://gw.example.com"),
            ),
            "key",
        )
        .expect("构造 Anthropic 自定义端点失败");
        assert_eq!(custom.base_url, "https://gw.example.com");
    }

    /// 工厂构造不发起网络请求：直接断言 DynModel 上的 provider 名与模型 id。
    #[test]
    fn build_model_addresses_the_requested_model() {
        let model = build_model(
            &tier(
                ProviderKind::OpenAICompatible,
                "qwen3-max",
                Some("https://gw/v1"),
            ),
            "key",
        )
        .expect("构造 DynModel 失败");
        assert_eq!(model.name(), "openai");
        assert_eq!(model.id(), Some("qwen3-max"));
    }

    #[test]
    fn build_model_rejects_empty_model_name() {
        let err = build_model(&tier(ProviderKind::OpenAI, "  ", None), "key")
            .expect_err("空模型名应报错");
        assert!(matches!(err, Error::InvalidConfig(ref msg) if msg.contains("模型名")));
    }

    #[test]
    fn build_model_rejects_deepseek_with_base_url() {
        let err = build_model(
            &tier(
                ProviderKind::DeepSeek,
                "deepseek-v4-flash",
                Some("https://x"),
            ),
            "key",
        )
        .expect_err("DeepSeek + base_url 应报错");
        assert!(matches!(err, Error::InvalidConfig(ref msg) if msg.contains("openai_compatible")));
    }

    /// 本地可判定失败不依赖环境：未配置 key（api_key 为 None）时
    /// build_llm_port 直接报 MissingApiKey（文案指向设置页，ADR-0003）。
    #[test]
    fn build_llm_port_requires_api_key() {
        let t = tier(ProviderKind::OpenAI, "gpt-5.2", None);
        assert_eq!(t.api_key, None);
        match build_llm_port(&t) {
            Err(Error::MissingApiKey) => {}
            Ok(_) => panic!("期望 MissingApiKey，实际成功构造"),
            Err(other) => panic!("期望 MissingApiKey，实际 {other}"),
        }
    }

    /// 上限是端口构造期预设（complete 协议无法按调用携带）：带 cap 构造
    /// 的端口持有该值，build_llm_port 保持无上限（连通探针不设限）。
    /// 字段值会被 agent_builder 应用于 AgentBuilder::max_tokens（构建期
    /// 参数，与 preamble 同链），此处断言预设语义。
    #[test]
    fn port_carries_configured_output_cap() {
        let model = build_model(
            &tier(
                ProviderKind::OpenAICompatible,
                "qwen3-max",
                Some("https://gw/v1"),
            ),
            "key",
        )
        .expect("构造 DynModel 失败");
        let capped = RigLlmPort {
            additional_params: None,
            model: model.clone(),
            max_tokens: Some(crate::llm::ANALYSIS_MAX_TOKENS),
        };
        assert_eq!(capped.max_tokens, Some(50_000));
        let uncapped = RigLlmPort {
            additional_params: None,
            model,
            max_tokens: None,
        };
        assert_eq!(uncapped.max_tokens, None);
    }

    /// 透传参数在端口构造期预设（complete 协议无法按调用携带）：
    /// build_llm_port_with 持有该值、build_llm_port_with_cap 保持 None。
    /// 参数内容即分析端口实际透传的
    /// [`crate::ingest::analysis_passthrough_params`]（reasoning_effort
    /// low + reasoning_split true——MiniMax M3.1 强制思考，thinking
    /// disabled 会 400），此处一并断言防漂移。
    #[test]
    fn additional_params_round_trip_through_constructor() {
        let t = ModelTier {
            api_key: Some("test-key".to_string()),
            ..tier(
                ProviderKind::OpenAICompatible,
                "MiniMax-M3.1-Flash",
                Some("http://x/v1"),
            )
        };
        let params = crate::ingest::analysis_passthrough_params();
        assert_eq!(
            params,
            serde_json::json!({ "reasoning_effort": "low", "reasoning_split": true }),
            "分析端口的透传参数必须是 reasoning_effort low + reasoning_split true"
        );

        let with = build_llm_port_with(&t, Some(ANALYSIS_MAX_TOKENS), Some(params.clone()))
            .expect("带 key 构造应成功");
        assert_eq!(with.additional_params.as_ref(), Some(&params));
        assert_eq!(with.max_tokens, Some(ANALYSIS_MAX_TOKENS));

        let without = build_llm_port_with_cap(&t, None).expect("构造应成功");
        assert_eq!(without.additional_params, None);
    }
}
