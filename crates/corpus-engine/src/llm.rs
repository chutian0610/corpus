//! LlmPort：引擎到 LLM 的窄端口 trait + 「测试连通」协议。
//!
//! rig 0.x 每个 minor 都破坏 API 且无稳定公开 mock trait（T2 调研笔记
//! 第 4 节），因此引擎依赖的只是这个手写窄端口：
//! - 真实实现 [`crate::rig_llm::RigLlmPort`] 在 `rig` feature 之后；
//! - 测试用手写 Fake 注入，协议完全离线、确定性。
//!
//! 这是隔离上游破坏性变更的常规防线：rig 升级只改一个文件。

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// LLM 端口：一次文本补全（可选 system prompt + user prompt）。
///
/// object-safe（`Box<dyn LlmPort>` / `&dyn LlmPort`），实现必须
/// `Send + Sync`（Tauri 线程池上跨线程调用）。
#[async_trait::async_trait]
pub trait LlmPort: Send + Sync {
    /// 请求一次补全：`system` 是可选的系统指令（Rig 实现映射到
    /// `AgentBuilder::preamble`），`user` 是用户消息；返回模型输出文本。
    async fn complete(&self, system: Option<&str>, user: &str) -> Result<String>;

    /// 流式补全：模型的文本增量一到达就经 `on_delta` 回调，返回值仍是
    /// 聚合后的完整输出（与 [`LlmPort::complete`] 语义一致）。
    ///
    /// 默认实现回退到 [`LlmPort::complete`]，把整体结果作为一次
    /// `on_delta` 回调——既有实现者（含 `--no-default-features` 的测试
    /// 路径）零改动获得该接口；支持真流式的实现（如
    /// [`crate::rig_llm::RigLlmPort`]）覆写之。回调要求 `Send`：流式
    /// future 会被装箱跨线程轮询（Tauri 线程池）。
    async fn complete_stream(
        &self,
        system: Option<&str>,
        user: &str,
        on_delta: &mut (dyn for<'s> FnMut(&'s str) + Send),
    ) -> Result<String> {
        let output = self.complete(system, user).await?;
        on_delta(&output);
        Ok(output)
    }
}

/// 连通测试的结果快照。
#[derive(Debug, Clone, Serialize)]
pub struct ConnectionReport {
    /// 从发出请求到收到回复的耗时（毫秒）。
    pub latency_ms: u64,
    /// 模型回复样例（截断后的原文）。
    pub reply: String,
}

/// 连通测试用的 system 指令：约束模型只回一个词，最小化 token 消耗。
pub const TEST_SYSTEM_PROMPT: &str =
    "You are a connectivity probe. Answer with exactly the requested token and nothing else.";

/// 连通测试用的 user 消息。
pub const TEST_USER_PROMPT: &str = "Reply with exactly: OK";

/// 连通测试回复在报告中的最大保留长度。
const REPORT_REPLY_MAX_CHARS: usize = 200;

// ---------------------------------------------------------------------------
// 调用统计（LlmCallStats）
// ---------------------------------------------------------------------------

/// LLM 档位（Ingest 两步各一档：fast 分析 / strong 成文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmTier {
    /// fast 档：第一步结构化分析。
    Fast,
    /// strong 档：第二步成文。
    Strong,
}

/// LLM 调用用途（与档位一一对应：fast 只用于分析、strong 只用于成文）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCallPurpose {
    /// 第一步：fast 档结构化分析。
    Analyze,
    /// 第二步：strong 档成文。
    Compose,
}

/// 单次 LLM 调用的结果分类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmCallOutcome {
    /// 输出解析成功，结果被管道采纳。
    Ok,
    /// 输出无法解析为引擎期望的结构（JSON 违约）。
    ParseFailed,
    /// 输出超过字符熔断上限（疑似模型复述原文或陷入重复）。
    Oversized,
    /// 端口调用本身失败（网络 / 鉴权 / 远端错误）。
    Error,
}

/// 单次 LLM 调用的统计快照。
///
/// 隐私设计：**不含任何 prompt / 输出原文**，只有长度与耗时——因此可以
/// 安全落盘为 Vault 的 `.corpus/llm-calls.log` JSON 行（该文件在
/// `.corpus/.gitignore` 忽略范围内），事后可 grep 排查「模型到底吐了
/// 多少字、卡在哪一步」。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmCallStats {
    /// 调用完成时刻（RFC 3339，本地时区）。
    pub ts: String,
    /// 档位（fast / strong）。
    pub tier: LlmTier,
    /// 用途（analyze / compose）。
    pub purpose: LlmCallPurpose,
    /// 第几次尝试（1 起；解析失败 / 超限会重试一次）。
    pub attempt: usize,
    /// system 消息字符数（无 system 为 0）。
    pub system_chars: usize,
    /// user 消息字符数（重试会追加约束句，按实际发送的消息计）。
    pub user_chars: usize,
    /// 模型输出字符数（以端口返回的聚合文本为准）。
    pub output_chars: usize,
    /// 本次调用耗时（毫秒）。
    pub duration_ms: u64,
    /// 结果分类。
    pub outcome: LlmCallOutcome,
    /// 失败原因摘要（截断 200 字；成功时无此字段）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// 分析步（fast 档）的服务端输出上限（token）。
///
/// 换算依据：分析输出熔断是 30,000 字符（见 [`crate::ingest`]），中文约
/// 1.5 字/token，30,000 ÷ 1.5 = 20,000。这是 rig 端口 `AgentBuilder::
/// max_tokens` 的第一道闸（服务端硬顶）；引擎侧字符熔断是第二道闸。
pub const ANALYSIS_MAX_TOKENS: u64 = 20_000;

/// 成文步（strong 档）的服务端输出上限（token）。
///
/// 换算依据：成文输出熔断是 60,000 字符（见 [`crate::ingest`]），
/// 60,000 ÷ 1.5 = 40,000。
pub const COMPOSE_MAX_TOKENS: u64 = 40_000;

/// 对注入的 [`LlmPort`] 发一次最小补全并计时（协议层，可离线用 Fake 驱动）。
///
/// 真实版本 [`crate::rig_llm::test_connection`]（`rig` feature）由配置
/// 构造 Rig 端口后委托本函数。
pub async fn test_connection_with(port: &dyn LlmPort) -> Result<ConnectionReport> {
    let start = Instant::now();
    let reply = port
        .complete(Some(TEST_SYSTEM_PROMPT), TEST_USER_PROMPT)
        .await?;
    Ok(ConnectionReport {
        latency_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        reply: truncate(&reply, REPORT_REPLY_MAX_CHARS),
    })
}

/// 按字符数截断（Unicode 安全）。
fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut truncated: String = text.chars().take(max_chars).collect();
        truncated.push('…');
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use std::sync::Mutex;

    /// 手写 Fake：记录收到的调用并回放固定脚本（见模块文档：rig 无稳定
    /// mock trait，测试不走真实网络）。
    struct FakeLlmPort {
        calls: Mutex<Vec<(Option<String>, String)>>,
        script: String,
    }

    impl FakeLlmPort {
        fn ok(script: &str) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                script: script.to_string(),
            }
        }

        fn recorded_calls(&self) -> Vec<(Option<String>, String)> {
            self.calls.lock().expect("fake 调用记录锁中毒").clone()
        }
    }

    #[async_trait::async_trait]
    impl LlmPort for FakeLlmPort {
        async fn complete(&self, system: Option<&str>, user: &str) -> Result<String> {
            self.calls
                .lock()
                .expect("fake 调用记录锁中毒")
                .push((system.map(str::to_string), user.to_string()));
            Ok(self.script.clone())
        }
    }

    #[tokio::test]
    async fn test_connection_sends_probe_prompts_and_reports_reply() {
        let fake = FakeLlmPort::ok("OK");
        let report = test_connection_with(&fake).await.expect("连通测试失败");

        // 协议：恰好一次调用，system/user 为探针常量
        let calls = fake.recorded_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0.as_deref(), Some(TEST_SYSTEM_PROMPT));
        assert_eq!(calls[0].1, TEST_USER_PROMPT);
        // 报告：回复原文透传，延迟非负（必然 >= 0，这里确认字段可用）
        assert_eq!(report.reply, "OK");
        let _ = report.latency_ms;
    }

    #[tokio::test]
    async fn test_connection_relays_port_error() {
        struct FailingPort;
        #[async_trait::async_trait]
        impl LlmPort for FailingPort {
            async fn complete(&self, _system: Option<&str>, _user: &str) -> Result<String> {
                Err(Error::Llm("模拟远端故障".into()))
            }
        }
        let err = test_connection_with(&FailingPort)
            .await
            .expect_err("应透传错误");
        assert!(matches!(err, Error::Llm(msg) if msg.contains("模拟远端故障")));
    }

    /// 默认流式实现回退 complete：整体结果作为一次 on_delta 回调，
    /// 返回值与非流式完全一致（只实现 complete 的 port 零改动可用）。
    #[tokio::test]
    async fn default_complete_stream_falls_back_to_single_delta() {
        let fake = FakeLlmPort::ok("分段不了的整体回复");
        let mut deltas: Vec<String> = Vec::new();
        let output = fake
            .complete_stream(Some("s"), "u", &mut |delta| {
                deltas.push(delta.to_string());
            })
            .await
            .expect("默认流式实现应成功");
        assert_eq!(output, "分段不了的整体回复");
        assert_eq!(deltas, vec!["分段不了的整体回复".to_string()]);
    }

    #[tokio::test]
    async fn long_reply_is_truncated_in_report() {
        let long_reply = "x".repeat(1000);
        let fake = FakeLlmPort::ok(&long_reply);
        let report = test_connection_with(&fake).await.expect("连通测试失败");
        assert!(report.reply.chars().count() <= 201, "超长回复应被截断");
        assert!(report.reply.ends_with('…'));
    }

    #[test]
    fn truncate_is_unicode_safe() {
        assert_eq!(truncate("中文回复", 2), "中文…");
        assert_eq!(truncate("ab", 5), "ab");
    }
}
