//! Ingest 管道：把一个 raw source 编译进 Wiki（票据 T3，核心 tracer）。
//!
//! 两段式公共 API（Apply 语义见 CONTEXT.md「Apply」）：
//! - [`ingest`]（第一步，不落盘）：读取 raw 并计算 SHA-256，指纹未变则
//!   跳过整个管道（零 LLM 调用）；fast 档结构化分析（容错提取 + 失败
//!   重试一次，再失败报错且零写入）→ 精确名去重（title/alias 与既有页
//!   frontmatter 不区分大小写匹配 → 更新而非新建）→ strong 档一次调用
//!   为 Source page 与每个 Entity/Concept 成文 → 组装 [`IngestDraft`]
//!   （内存变更集，可整体检视/丢弃，不产生任何写入与提交）；
//! - [`apply`]（第二步，显式落盘）：全页面原子写 + git add/commit
//!   （恰好一个提交）+ log.md 追加 + index.md 增量更新 + 指纹记录；
//!   任一步失败恢复快照并清空暂存区，不留半写状态。
//!   自动执行还是人审触发由配置 `apply_mode` 决定，引擎不预设——
//!   壳层读到 `manual` 时不调 apply，Draft 留待人审。
//!
//! LLM 输出治理（双层闸）：
//! - 服务端硬顶：端口构造时按档位设 `max_tokens`（rig 侧，见
//!   [`crate::llm::ANALYSIS_MAX_TOKENS`] / [`crate::llm::COMPOSE_MAX_TOKENS`]）；
//! - 引擎熔断：[`with_retry_stream`] 对聚合输出做字符上限检查（分析
//!   30,000 / 成文 60,000，常量可调），超限即视为该次失败、不进入
//!   解析——防模型复述原文 / 陷入重复把分析阶段拖成数万字。
//!
//! 解析前剥离思考块：推理模型（如 MiniMax-M3）经 OpenAI 兼容端点默认
//! 开思考，`<think>…</think>` 文本可能内联在输出流里——不剥离会污染
//! first-`{` 到 last-`}` 的 JSON 提取窗口（见 [`strip_think_blocks`]）。
//! 剥离只影响解析；`chars` 计数保持原样（如实反映模型输出量）。
//!
//! 全程每次 LLM 调用都记 [`LlmCallStats`]（档位/用途/字数/耗时/结果，
//! 无原文；失败时附输出首尾片段诊断），随 [`IngestReport`] 返回并即时
//! 追加到 Vault 的 `.corpus/llm-calls.log`（JSON 行，被 .gitignore 忽略）。
//!
//! 引擎不依赖 tauri：进度经 `on_progress: impl FnMut(IngestStage)` 回调
//! 上报（见 T3 调研笔记第 4 节架构提示），壳层负责转成事件。

use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::atomic::atomic_write;
use crate::error::{Error, Result};
use crate::fingerprint::{sha256_hex, StateStore};
use crate::git;
use crate::llm::{LlmCallOutcome, LlmCallPurpose, LlmCallStats, LlmPort, LlmTier};
use crate::page::{
    parse_page, slugify, wikilink, PageAction, PageDraft, PageFrontmatter, PageKind,
};
use crate::template::{template_by_id, WikiTemplate};
use crate::vault::vault_template_id;

/// Ingest 提交信息前缀（`corpus: ingest | <Source 标题>`，可 grep）。
pub const INGEST_COMMIT_PREFIX: &str = "corpus: ingest";

/// 进度阶段：粗粒度、低频（读取/分析/生成/落盘），壳层映射为 UI 步骤。
///
/// 分析/生成两步的 LLM 输出经流式增量上报：同阶段会重复发出携带递增
/// `chars`（模型已输出字符数）的事件（节流见 [`with_retry_stream`]）；
/// `chars: 0` 表示阶段开始。增量只报字数不报内容——分析输出是 JSON、
/// 生成输出会剧透 Wiki 正文，展示原文无意义且可能闪烁。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum IngestStage {
    /// 读取 raw 文件并计算指纹。
    Reading {
        /// raw 文件相对 Vault 的路径。
        raw: String,
    },
    /// 第一步：fast 档结构化分析（流式）。
    Analyzing {
        /// 模型已输出的字符数（0 = 阶段开始）。
        chars: usize,
    },
    /// 第二步：strong 档成文（流式；`pages` 为计划页面数）。
    Generating {
        /// 计划生成正文的页面数。
        pages: usize,
        /// 模型已输出的字符数（0 = 阶段开始）。
        chars: usize,
    },
    /// 落盘：原子写 + git 提交 + log/index 更新。
    Applying {
        /// Draft 中的页面数。
        pages: usize,
    },
}

/// 第一步（fast 档）的 JSON 输出目标：实体/概念列表与 Source 元信息。
///
/// 仅本模块内部使用（解析 LLM 输出），不属于公共 API。
#[derive(Debug, Clone, Default, Deserialize)]
struct AnalysisOutcome {
    /// Source page 标题（空则引擎回退 raw 文件名）。
    #[serde(default)]
    source_title: String,
    /// 原始文档摘要（供第二步成文）。
    #[serde(default)]
    source_summary: String,
    /// 实体页面清单。
    #[serde(default)]
    entities: Vec<AnalyzedPage>,
    /// 概念页面清单。
    #[serde(default)]
    concepts: Vec<AnalyzedPage>,
}

/// 第一步产出的单个页面条目（仅本模块内部使用）。
#[derive(Debug, Clone, Default, Deserialize)]
struct AnalyzedPage {
    /// 页面标题（必填，空则引擎丢弃该条目）。
    #[serde(default)]
    title: String,
    /// 别名（跨语言 / 缩写）。
    #[serde(default)]
    aliases: Vec<String>,
    /// 标签。
    #[serde(default)]
    tags: Vec<String>,
    /// 该条目在原文中的要点（供第二步成文）。
    #[serde(default)]
    summary: String,
    /// 相关页面标题（引擎解析为 wikilink）。
    #[serde(default)]
    related: Vec<String>,
}

/// 第二步（strong 档）的 JSON 输出目标：每个计划页面的正文。
#[derive(Debug, Clone, Default, Deserialize)]
struct WrittenPages {
    #[serde(default)]
    pages: Vec<WrittenPage>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct WrittenPage {
    #[serde(default)]
    path: String,
    #[serde(default)]
    body: String,
}

/// 降级页标注：第二步 LLM 漏产出该页正文、以分析摘要兜底时的显式标记
/// （产出索引与 Draft 检视清单都会带上，可 grep）。
pub const DEGRADED_NOTE: &str = "（降级：模型未产出，摘要兜底）";

/// Source page 的原文指针：raw 相对路径 + 内容 SHA-256（frontmatter 的
/// `raw_path` / `fingerprint` 专有字段对，仅 Source page 使用）。
#[derive(Debug, Clone, Copy)]
pub struct RawPointer<'a> {
    /// raw source 在 Vault 内的相对路径。
    pub raw_path: &'a str,
    /// raw source 内容的 SHA-256。
    pub fingerprint: &'a str,
}

/// Draft：一次 Ingest 的内存变更集，可整体检视或丢弃（[`IngestDraft::discard`]）。
#[derive(Debug, Clone)]
pub struct IngestDraft {
    /// Source page 标题。
    pub source_title: String,
    /// raw 文件相对 Vault 的路径。
    pub raw_path: String,
    /// raw 内容 SHA-256。
    pub fingerprint: String,
    /// 全部页面变更（Source 在首位）。
    pub pages: Vec<PageDraft>,
    /// 使用摘要兜底正文的页面路径（第二步 LLM 漏产出的降级页）。
    pub degraded_paths: Vec<String>,
    /// 本次 Ingest 全部 LLM 调用统计（分析 + 成文，含重试）。
    pub llm_calls: Vec<LlmCallStats>,
}

impl IngestDraft {
    /// 人读检视清单：每页一行 `action path title`，降级页带 [`DEGRADED_NOTE`]。
    pub fn summary_lines(&self) -> Vec<String> {
        let mut lines = vec![format!(
            "source: {} (raw: {}, sha256: {})",
            self.source_title, self.raw_path, self.fingerprint
        )];
        for page in &self.pages {
            let mut line = format!(
                "{:?} {}: {}",
                page.action, page.path, page.frontmatter.title
            );
            if self.degraded_paths.contains(&page.path) {
                line.push_str(DEGRADED_NOTE);
            }
            lines.push(line);
        }
        lines
    }

    /// 报告形态：产出页面清单 + Apply 产生的 commit（未 Apply 时 `None`）。
    pub fn report(&self, commit: Option<String>) -> IngestReport {
        IngestReport {
            skipped: false,
            fingerprint: self.fingerprint.clone(),
            source_title: self.source_title.clone(),
            pages: self
                .pages
                .iter()
                .map(|p| IngestedPage {
                    path: p.path.clone(),
                    title: p.frontmatter.title.clone(),
                    page_type: p.frontmatter.page_type,
                    action: p.action,
                })
                .collect(),
            commit,
            llm_calls: self.llm_calls.clone(),
        }
    }

    /// 显式丢弃 Draft：内存变更集直接释放，Wiki 零改动。
    pub fn discard(self) {}
}

/// [`ingest`] 的结果：指纹命中跳过，或待 Apply 的 Draft。
#[derive(Debug, Clone)]
pub enum IngestOutcome {
    /// 指纹命中，管道跳过（零 LLM 调用、零写入）。
    Skipped(IngestReport),
    /// 待 Apply 的内存变更集（尚未落盘、尚未提交）。
    Draft(IngestDraft),
}

/// Ingest 结果快照（含跳过路径）。
#[derive(Debug, Clone, Serialize)]
pub struct IngestReport {
    /// 指纹命中，管道被跳过（零 LLM 调用、零写入）。
    pub skipped: bool,
    /// 本次 raw 内容的 SHA-256。
    pub fingerprint: String,
    /// Source page 标题。
    pub source_title: String,
    /// 产出 / 更新的页面清单。
    pub pages: Vec<IngestedPage>,
    /// Apply 产生的 git 短 hash（skipped 与未 Apply 时为 `None`）。
    pub commit: Option<String>,
    /// 本次 Ingest 的 LLM 调用统计（跳过路径为空——零调用）。
    pub llm_calls: Vec<LlmCallStats>,
}

/// 单个产出页面。
#[derive(Debug, Clone, Serialize)]
pub struct IngestedPage {
    /// 相对 `wiki/` 的路径。
    pub path: String,
    /// 页面标题。
    pub title: String,
    /// 页面类型。
    pub page_type: PageKind,
    /// 新建还是更新。
    pub action: PageAction,
}

/// 摄取一个 raw 文件（第一步，不落盘）：指纹检查 → 两步 LLM → Draft。
///
/// 全程零写入、零提交——Draft 是内存变更集，落盘由显式 [`apply`] 完成
/// （`apply_mode = manual` 时壳层不调 apply，Draft 留待人审）。
///
/// `raw_path` 接受绝对路径或相对 `vault_root` 的路径，必须位于 Vault 的
/// `raw/` 目录内（否则 [`Error::RawNotInVault`]）。`fast` / `strong` 分别
/// 是两步所用档位的 [`LlmPort`]（注入方决定真实或 fake）。
///
/// `on_progress` 要求 `Send`：LLM 流式 future 跨线程轮询（见
/// [`LlmPort::complete_stream`]），进度回调随之进入 `Send` 上下文。
pub async fn ingest<F>(
    vault_root: impl AsRef<Path>,
    raw_path: impl AsRef<Path>,
    fast: &dyn LlmPort,
    strong: &dyn LlmPort,
    mut on_progress: F,
) -> Result<IngestOutcome>
where
    F: FnMut(IngestStage) + Send,
{
    let vault_root = vault_root.as_ref();
    if !vault_root.join("wiki").is_dir() {
        return Err(Error::NotAVault {
            path: vault_root.to_path_buf(),
            reason: "缺 wiki/ 目录".to_string(),
        });
    }
    let (raw_rel, raw_abs) = resolve_raw(vault_root, raw_path.as_ref())?;
    let raw_rel = raw_rel.to_string_lossy().replace('\\', "/");

    on_progress(IngestStage::Reading {
        raw: raw_rel.clone(),
    });
    let bytes = fs::read(&raw_abs)?;
    let raw_text = String::from_utf8(bytes.clone()).map_err(|_| {
        Error::InvalidConfig(format!("raw 文件不是 UTF-8 文本：{}", raw_abs.display()))
    })?;
    let fingerprint = sha256_hex(&bytes);

    // 指纹命中：跳过整个管道（零 LLM 调用）。
    let state = StateStore::open(vault_root)?;
    if state.fingerprint(&raw_rel)?.as_deref() == Some(fingerprint.as_str()) {
        return Ok(IngestOutcome::Skipped(IngestReport {
            skipped: true,
            fingerprint,
            source_title: existing_source_title(vault_root, &raw_rel)
                .unwrap_or_else(|| raw_file_stem(&raw_rel)),
            pages: Vec::new(),
            commit: None,
            llm_calls: Vec::new(),
        }));
    }

    let template_id = vault_template_id(vault_root)?;
    let template = template_by_id(&template_id).ok_or_else(|| Error::UnknownTemplate {
        id: template_id.clone(),
        available: crate::template::builtin_templates()
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    })?;

    // LLM 调用统计收集器：全程累积并即时落 llm-calls.log（见类型文档）。
    let mut recorder = LlmCallRecorder::new(vault_root);

    // 第一步：结构化分析（fast 档，流式增量经节流上报 chars）。
    on_progress(IngestStage::Analyzing { chars: 0 });
    let mut analysis = analyze(fast, template, &raw_text, &mut recorder, &mut |chars| {
        on_progress(IngestStage::Analyzing { chars });
    })
    .await?;
    if analysis.source_title.trim().is_empty() {
        analysis.source_title = raw_file_stem(&raw_rel);
    }

    // 组装页面计划（精确名去重 + 别名解析）。
    let existing = scan_existing_pages(vault_root)?;
    let plan = build_plan(&analysis, &raw_rel, &existing)?;

    // 第二步：成文（strong 档，流式增量经节流上报 chars）。
    on_progress(IngestStage::Generating {
        pages: plan.len(),
        chars: 0,
    });
    let bodies = compose(
        strong,
        template,
        &raw_text,
        &raw_rel,
        &plan,
        &mut recorder,
        &mut |chars| {
            on_progress(IngestStage::Generating {
                pages: plan.len(),
                chars,
            })
        },
    )
    .await?;

    let llm_calls = recorder.finish();
    Ok(IngestOutcome::Draft(build_draft(
        &raw_rel,
        plan,
        &bodies,
        &fingerprint,
        llm_calls,
    )))
}

/// Apply（第二步，显式落盘）：全部页面原子写 + log/index 更新 + 恰好
/// 一个 git 提交 + 指纹记录。
///
/// 任一步失败恢复快照并 `git reset` 清空暂存区，文件系统与 git 回到
/// 提交点。指纹只在提交成功后记录——Apply 失败的 raw 可直接重摄取。
pub fn apply<F>(
    vault_root: impl AsRef<Path>,
    draft: &IngestDraft,
    mut on_progress: F,
) -> Result<String>
where
    F: FnMut(IngestStage),
{
    let vault_root = vault_root.as_ref();
    on_progress(IngestStage::Applying {
        pages: draft.pages.len(),
    });
    let commit = write_and_commit(vault_root, draft)?;
    StateStore::open(vault_root)?.set_fingerprint(&draft.raw_path, &draft.fingerprint)?;
    Ok(commit)
}

/// 归一化 raw 路径：相对 Vault 解析为绝对，并校验位于 `raw/` 内。
fn resolve_raw(vault_root: &Path, raw: &Path) -> Result<(PathBuf, PathBuf)> {
    let abs = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        vault_root.join(raw)
    };
    // 不 canonicalize（文件必须已存在，join 结果足够判定前缀）；
    // raw 不存在时随后的 read 会报 IO 错误。
    let rel = abs
        .strip_prefix(vault_root)
        .map_err(|_| Error::RawNotInVault {
            raw: raw.to_path_buf(),
            vault: vault_root.to_path_buf(),
        })?
        .to_path_buf();
    if !rel.starts_with("raw") {
        return Err(Error::RawNotInVault {
            raw: raw.to_path_buf(),
            vault: vault_root.to_path_buf(),
        });
    }
    Ok((rel, abs))
}

/// raw 相对路径的文件名主干（Source 标题回退用）。
fn raw_file_stem(rel: &str) -> String {
    Path::new(rel)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_string())
}

/// 已有 Source page 中锚定同一 raw 的标题（跳过报告展示用）。
fn existing_source_title(vault_root: &Path, raw_rel: &str) -> Option<String> {
    let dir = vault_root.join("wiki/sources");
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok((fm, _)) = parse_page(&content) {
                if fm.raw_path.as_deref() == Some(raw_rel) {
                    return Some(fm.title);
                }
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// 第一步：结构化分析（fast 档）
// ---------------------------------------------------------------------------

/// 第一步 system 指令的 JSON 输出约束（追加在模板 schema_prompt 之后）。
const ANALYSIS_JSON_INSTRUCTIONS: &str = "\
Output contract — respond with ONE JSON object and nothing else (no prose, \
no code fences). Shape:
{
  \"source_title\": \"title for the source page (original title or topic)\",
  \"source_summary\": \"3-6 sentence summary of the raw source\",
  \"entities\": [{\"title\": \"...\", \"aliases\": [\"...\"], \"tags\": [\"...\"], \"summary\": \"key points about this entity in the source\", \"related\": [\"titles of related pages\"]}],
  \"concepts\": [{\"title\": \"...\", \"aliases\": [\"...\"], \"tags\": [\"...\"], \"summary\": \"...\", \"related\": [\"...\"]}]
}
Every item MUST have a non-empty title; aliases must contain at least the \
title itself when no better alias exists; use [] when a list is empty.";

/// 流式进度节流：距上次上报至少这么久（时间阈值）……
const REPORT_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
/// ……或累计新增至少这么多字符（字符阈值），二者满足其一才上报。
const REPORT_MIN_NEW_CHARS: usize = 50;

/// 第一步（分析）的输出熔断上限（字符）。正常分析 JSON 应在几千字内，
/// 远小于此值；超过即视为模型复述原文 / 陷入重复，该次调用按失败处理。
/// 可按需调整（服务端 max_tokens 是第一道闸，这是引擎侧第二道）。
pub const ANALYSIS_MAX_OUTPUT_CHARS: usize = 30_000;

/// 第二步（成文）的输出熔断上限（字符）。可按需调整。
pub const COMPOSE_MAX_OUTPUT_CHARS: usize = 60_000;

/// 统计与日志文件的相对路径（Vault 内；被 `.corpus/.gitignore` 忽略）。
pub const LLM_CALLS_LOG_REL: &str = ".corpus/llm-calls.log";

/// 错误摘要写入统计时的最大保留长度（字符）。
const STATS_ERROR_MAX_CHARS: usize = 200;

/// 失败诊断片段长度：输出首尾各保留的字符数（进 [`LlmCallStats`] 的
/// `output_head` / `output_tail`，见 llm.rs 隐私取舍说明）。
const STATS_SNIPPET_CHARS: usize = 300;

/// 错误摘要末尾摘录的长度：开头 / 结尾各保留的字符数（单行化）。
const ERROR_EXCERPT_CHARS: usize = 80;

/// 上次输出超长时，重试 user 消息末尾追加的强约束。
const RETRY_CONSTRAINT_OVERSIZED: &str =
    "上次输出超长。只输出符合要求的紧凑内容，禁止复述原文、禁止重复。";

/// 上次输出无法解析时，重试 user 消息末尾追加的强约束。
const RETRY_CONSTRAINT_UNPARSEABLE: &str =
    "上次输出无法解析。只输出符合要求的紧凑内容，禁止复述原文、禁止重复。";

/// 一次调用的画像：档位 + 用途 + 步骤名 + 字符熔断上限（分析/成文各自
/// 一份，[`with_retry_stream`] 据此填统计与判断超限）。
struct CallProfile {
    tier: LlmTier,
    purpose: LlmCallPurpose,
    /// 步骤名（进错误消息，如「结构化分析」「成文」）。
    what: &'static str,
    max_output_chars: usize,
}

/// LLM 调用统计收集器：ingest 全程累积，同时把每条 [`LlmCallStats`]
/// 即时追加为 [`LLM_CALLS_LOG_REL`] 的一行 JSON——即使摄取失败报错
/// （无法返回报告），日志里也已留下每次调用的长度与耗时供排查。
pub(crate) struct LlmCallRecorder {
    vault_root: PathBuf,
    calls: Vec<LlmCallStats>,
}

impl LlmCallRecorder {
    fn new(vault_root: &Path) -> Self {
        Self {
            vault_root: vault_root.to_path_buf(),
            calls: Vec::new(),
        }
    }

    /// 记录一条统计：追加日志行（best-effort，失败只打 stderr 不阻断
    /// 摄取——诊断数据不能反噬主流程）+ 累积进内存（进报告）。
    fn record(&mut self, stats: LlmCallStats) {
        if let Err(err) = append_llm_call_line(&self.vault_root, &stats) {
            eprintln!("追加 LLM 调用日志失败（不影响摄取结果）：{err}");
        }
        self.calls.push(stats);
    }

    /// 收束为报告字段。
    fn finish(self) -> Vec<LlmCallStats> {
        self.calls
    }
}

/// 追加一行 JSON 到 Vault 的 llm-calls.log（不存在则创建）。
fn append_llm_call_line(vault_root: &Path, stats: &LlmCallStats) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(vault_root.join(LLM_CALLS_LOG_REL))?;
    writeln!(file, "{}", serde_json::to_string(stats)?)?;
    Ok(())
}

/// LLM 流式调用 + 输出熔断 + 解析的重试助手（两步共用）：
///
/// - **熔断**：聚合输出超过 `profile.max_output_chars` 即视为该次失败
///   （不进入 parse——超限内容即使是合法 JSON 也不采信），原因写明
///   「输出 N 字超过上限 M，疑似模型复述原文或陷入重复」；
/// - **重试**：解析失败 / 超限均重试一次，重试的 user 消息末尾按上次
///   失败原因追加强约束句（[`RETRY_CONSTRAINT_OVERSIZED`] /
///   [`RETRY_CONSTRAINT_UNPARSEABLE`]）；两次失败报 [`Error::BadLlmOutput`]
///   （`what` 为步骤名，错误含两次输出字数）；端口调用错误（网络 /
///   鉴权 / 远端）记录统计后原样透传，不重试（与既有语义一致——
///   重试只针对输出质量问题）；
/// - **统计**：每次尝试记一条 [`LlmCallStats`]（`recorder` 落日志 +
///   进报告）。解析失败 / 超限的记录额外附失败诊断（输出首尾片段 +
///   思考标签检测，见 [`fill_failure_diagnostics`]），错误摘要末尾附
///   开头/结尾摘录（[`error_with_excerpt`]）——截断点与思考混流一眼
///   可见；端口调用错误（无输出）不记片段。
///
/// 文本增量经 [`LlmPort::complete_stream`] 即时到达，`on_chars` 是节流后
/// 的 chars 上报（模型已输出字符数）：距上次上报 ≥[`REPORT_MIN_INTERVAL`]
/// 或累计新增 ≥[`REPORT_MIN_NEW_CHARS`] 才放行——连续小增量合并成一报，
/// 避免高频回调淹没进度通道。流结束后补一次终值（尾部增量可能未达阈值）。
/// 每次尝试独立计数（重试的输出与上次无关）。
async fn with_retry_stream<T>(
    recorder: &mut LlmCallRecorder,
    port: &dyn LlmPort,
    system: Option<&str>,
    user: &str,
    profile: CallProfile,
    parse: impl Fn(&str) -> Result<T>,
    on_chars: &mut (dyn FnMut(usize) + Send),
) -> Result<T> {
    let CallProfile {
        tier,
        purpose,
        max_output_chars,
        what,
    } = profile;
    let system_chars = system.map_or(0, |s| s.chars().count());
    let mut last_error = String::new();
    let mut attempt_chars: Vec<usize> = Vec::with_capacity(2);
    // 上次失败后追加在 user 末尾的强约束（首次尝试为空）。
    let mut retry_note = String::new();
    for _attempt in 0..2 {
        let attempt_user = if retry_note.is_empty() {
            user.to_string()
        } else {
            format!("{user}\n\n{retry_note}")
        };
        let start = std::time::Instant::now();
        let mut chars = 0usize;
        let mut reported_chars = 0usize;
        let mut last_report = std::time::Instant::now();
        let output = match port
            .complete_stream(system, &attempt_user, &mut |delta| {
                chars += delta.chars().count();
                if last_report.elapsed() >= REPORT_MIN_INTERVAL
                    || chars - reported_chars >= REPORT_MIN_NEW_CHARS
                {
                    on_chars(chars);
                    reported_chars = chars;
                    last_report = std::time::Instant::now();
                }
            })
            .await
        {
            Ok(output) => output,
            Err(err) => {
                recorder.record(LlmCallStats {
                    ts: chrono::Local::now().to_rfc3339(),
                    tier,
                    purpose,
                    attempt: attempt_chars.len() + 1,
                    system_chars,
                    user_chars: attempt_user.chars().count(),
                    output_chars: chars,
                    duration_ms: elapsed_ms(start),
                    outcome: LlmCallOutcome::Error,
                    error: Some(truncate_chars(&err.to_string(), STATS_ERROR_MAX_CHARS)),
                    // 端口调用本身失败：无模型输出可诊断
                    output_head: None,
                    output_tail: None,
                    has_think_tag: None,
                });
                return Err(err);
            }
        };
        if chars != reported_chars {
            on_chars(chars);
        }
        let output_chars = output.chars().count();
        attempt_chars.push(output_chars);
        let stats = |outcome: LlmCallOutcome, error: Option<String>| LlmCallStats {
            ts: chrono::Local::now().to_rfc3339(),
            tier,
            purpose,
            attempt: attempt_chars.len(),
            system_chars,
            user_chars: attempt_user.chars().count(),
            output_chars,
            duration_ms: elapsed_ms(start),
            outcome,
            error,
            output_head: None,
            output_tail: None,
            has_think_tag: None,
        };

        // 熔断：超限即失败，不进入 parse（即使内容恰好是合法 JSON）。
        if output_chars > max_output_chars {
            let reason = format!(
                "输出 {output_chars} 字超过上限 {max_output_chars}，疑似模型复述原文或陷入重复"
            );
            let mut record = stats(
                LlmCallOutcome::Oversized,
                Some(error_with_excerpt(&reason, &output)),
            );
            fill_failure_diagnostics(&mut record, &output);
            recorder.record(record);
            last_error = reason;
            retry_note = RETRY_CONSTRAINT_OVERSIZED.to_string();
            continue;
        }
        match parse(&output) {
            Ok(parsed) => {
                recorder.record(stats(LlmCallOutcome::Ok, None));
                return Ok(parsed);
            }
            Err(err) => {
                last_error = err.to_string();
                let mut record = stats(
                    LlmCallOutcome::ParseFailed,
                    Some(error_with_excerpt(&last_error, &output)),
                );
                fill_failure_diagnostics(&mut record, &output);
                recorder.record(record);
                retry_note = RETRY_CONSTRAINT_UNPARSEABLE.to_string();
            }
        }
    }
    Err(Error::BadLlmOutput(format!(
        "{what}输出两次均失败（第 1 次 {} 字，第 2 次 {} 字；最后一次：{last_error}）",
        attempt_chars[0], attempt_chars[1]
    )))
}

/// 已耗时（毫秒，u64 饱和截断）。
fn elapsed_ms(start: std::time::Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}

/// 取前 `max_chars` 个字符（Unicode 安全）。
fn head_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// 取后 `max_chars` 个字符（不足则原样；Unicode 安全）。
fn tail_chars(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_string()
    } else {
        text.chars().skip(count - max_chars).collect()
    }
}

/// 控制字符（换行 / 制表等）单行化为空格（摘录进悬停 title 用）。
fn single_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// 失败摘要：原因（截断 [`STATS_ERROR_MAX_CHARS`]）+ 末尾输出摘录
/// 「开头：⟨前 80 字单行化⟩…结尾：⟨后 80 字单行化⟩」——前端统计表
/// 悬停 title 直接可见截断点与思考混流证据，摘录整体不被截断。
fn error_with_excerpt(reason: &str, output: &str) -> String {
    format!(
        "{}；开头：{}…结尾：{}",
        truncate_chars(reason, STATS_ERROR_MAX_CHARS),
        single_line(&head_chars(output, ERROR_EXCERPT_CHARS)),
        single_line(&tail_chars(output, ERROR_EXCERPT_CHARS)),
    )
}

/// 失败诊断：填充输出首尾片段与思考标签检测（仅解析失败 / 超限路径
/// 调用；成功保持 `None` 且不序列化，隐私取舍见 [`LlmCallStats`]）。
fn fill_failure_diagnostics(stats: &mut LlmCallStats, output: &str) {
    stats.output_head = Some(head_chars(output, STATS_SNIPPET_CHARS));
    stats.output_tail = Some(tail_chars(output, STATS_SNIPPET_CHARS));
    stats.has_think_tag = Some(output.contains("<think>") || output.contains("</think>"));
}

/// 从输出中剥离推理模型的思考块（`<think>…</think>`），返回剥离结果
/// （无思考块时零拷贝借用原文）。
///
/// 规则：非贪婪、含多行——每对 `<think>…</think>` 整段移除（可多对）；
/// 未闭合的 `<think>` 从标签开头到输出末尾整段视为思考。推理模型经
/// OpenAI 兼容端点默认开思考，思考文本可能内联在输出流里，不剥离会
/// 污染 first-`{` 到 last-`}` 的 JSON 提取窗口。只影响解析：`chars`
/// 计数与 [`LlmCallStats::output_chars`] 保持原样（如实反映模型输出量）。
fn strip_think_blocks(output: &str) -> Cow<'_, str> {
    if !output.contains("<think>") {
        return Cow::Borrowed(output);
    }
    const OPEN: &str = "<think>";
    const CLOSE: &str = "</think>";
    let mut cleaned = String::with_capacity(output.len());
    let mut rest = output;
    while let Some(start) = rest.find(OPEN) {
        cleaned.push_str(&rest[..start]);
        let after_open = &rest[start + OPEN.len()..];
        match after_open.find(CLOSE) {
            // 闭合：跳过整对思考块，继续找下一对
            Some(close) => rest = &after_open[close + CLOSE.len()..],
            // 未闭合：开头到末尾整段视为思考，其后无正文
            None => return Cow::Owned(cleaned),
        }
    }
    cleaned.push_str(rest);
    Cow::Owned(cleaned)
}

/// 第一步调用：schema_prompt 注入 system，raw 全文作为 user；
/// 解析失败 / 超限重试一次，再失败报 [`Error::BadLlmOutput`]（零写入）。
/// `on_chars` 收到节流后的流式 chars 上报。
async fn analyze(
    fast: &dyn LlmPort,
    template: &WikiTemplate,
    raw: &str,
    recorder: &mut LlmCallRecorder,
    on_chars: &mut (dyn FnMut(usize) + Send),
) -> Result<AnalysisOutcome> {
    let system = format!("{}\n\n{ANALYSIS_JSON_INSTRUCTIONS}", template.schema_prompt);
    let user = format!("# Raw source\n\n```text\n{raw}\n```\n");
    with_retry_stream(
        recorder,
        fast,
        Some(&system),
        &user,
        CallProfile {
            tier: LlmTier::Fast,
            purpose: LlmCallPurpose::Analyze,
            what: "结构化分析",
            max_output_chars: ANALYSIS_MAX_OUTPUT_CHARS,
        },
        parse_analysis,
        on_chars,
    )
    .await
}

/// 容错解析分析输出：先剥离思考块，再截取首个 `{` 到最后一个 `}` 走 serde_json。
fn parse_analysis(output: &str) -> Result<AnalysisOutcome> {
    let stripped = strip_think_blocks(output);
    let json = extract_json_object(&stripped);
    serde_json::from_str(json).map_err(|err| Error::BadLlmOutput(format!("JSON 解析失败：{err}")))
}

/// 提取输出中的 JSON 对象字面量（容忍 ```json 围栏与前后杂文字）。
fn extract_json_object(output: &str) -> &str {
    match (output.find('{'), output.rfind('}')) {
        (Some(start), Some(end)) if end > start => &output[start..=end],
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// 页面计划：精确名去重与别名解析
// ---------------------------------------------------------------------------

/// 既有页面（wiki/ 下已存在的页面）。
#[derive(Debug, Clone)]
struct ExistingPage {
    /// 相对 wiki/ 的路径。
    path: String,
    frontmatter: PageFrontmatter,
    body: String,
}

/// 计划页面：分析条目 + 去重决策 + slug 目标。
#[derive(Debug, Clone)]
struct PlannedPage {
    kind: PageKind,
    title: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    /// 解析后的 related wikilink（`[[link]]` 形式）。
    related: Vec<String>,
    summary: String,
    /// 精确名命中既有页面 → 更新它（保留 path/created）。
    existing: Option<ExistingPage>,
    /// 相对 wiki/ 的目标路径（含 .md）。
    path: String,
}

/// 名称归一化：去首尾空白 + 小写（精确名匹配不区分大小写）。
fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

/// 别名并集的唯一实现（build_plan / 批内合并 / 更新并集 / 不变量兜底
/// 四处收敛于此）：把 `incoming` 去空白、去重后并入 `aliases`（跳过与
/// `title` 或既有项重复的别名，不区分大小写），并保证 `title` 自身在
/// 集合内——「aliases 非空不变量」由此兜底。
fn merge_aliases(aliases: &mut Vec<String>, incoming: &[String], title: &str) {
    let push_unique = |aliases: &mut Vec<String>, value: String| {
        let normalized = normalize_name(&value);
        if normalized != normalize_name(title)
            && !aliases.iter().any(|a| normalize_name(a) == normalized)
        {
            aliases.push(value);
        }
    };
    for alias in incoming {
        let alias = alias.trim().to_string();
        if alias.is_empty() {
            continue;
        }
        push_unique(aliases, alias);
    }
    if !aliases
        .iter()
        .any(|a| normalize_name(a) == normalize_name(title))
    {
        aliases.push(title.to_string());
    }
}

/// 扫描 wiki/ 下三类核心页面（解析失败的单页跳过，不阻断管道）。
fn scan_existing_pages(vault_root: &Path) -> Result<Vec<ExistingPage>> {
    let wiki = vault_root.join("wiki");
    let mut pages = Vec::new();
    for kind in [PageKind::Source, PageKind::Entity, PageKind::Concept] {
        let dir = wiki.join(kind.dir());
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue, // 目录尚不存在（全新 Vault）
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok((frontmatter, body)) = parse_page(&content) else {
                continue;
            };
            let rel = path
                .strip_prefix(&wiki)
                .expect("wiki 下的页面必然能 strip wiki/ 前缀");
            pages.push(ExistingPage {
                path: rel.to_string_lossy().replace('\\', "/"),
                frontmatter,
                body,
            });
        }
    }
    Ok(pages)
}

impl ExistingPage {
    /// 该页面占用的全部名称（title + aliases，归一化后）。
    fn owned_names(&self) -> Vec<String> {
        let mut names = vec![normalize_name(&self.frontmatter.title)];
        names.extend(self.frontmatter.aliases.iter().map(|n| normalize_name(n)));
        names
    }
}

/// 组装页面计划：Source page（按 raw 路径锚定复用）+ 实体/概念
/// （批内同名合并、与既有页面精确名去重、related 解析、slug 分配）。
fn build_plan(
    analysis: &AnalysisOutcome,
    raw_rel: &str,
    existing: &[ExistingPage],
) -> Result<Vec<PlannedPage>> {
    let mut planned: Vec<PlannedPage> = Vec::new();

    // Source page：身份键是 raw 相对路径（同一 raw 变更后重摄取更新原页）。
    let source_existing = existing
        .iter()
        .find(|p| p.frontmatter.raw_path.as_deref() == Some(raw_rel))
        .cloned();
    let source_title = source_existing
        .as_ref()
        .map(|p| p.frontmatter.title.clone())
        .unwrap_or_else(|| analysis.source_title.trim().to_string());
    planned.push(PlannedPage {
        kind: PageKind::Source,
        title: source_title.clone(),
        aliases: vec![source_title],
        tags: vec!["source".to_string()],
        related: Vec::new(),
        summary: analysis.source_summary.clone(),
        existing: source_existing,
        path: String::new(), // 下方统一分配
    });

    // 既有实体/概念页面索引（Source page 不参与精确名去重）。
    let existing_index: Vec<&ExistingPage> = existing
        .iter()
        .filter(|p| {
            matches!(
                p.frontmatter.page_type,
                PageKind::Entity | PageKind::Concept
            )
        })
        .collect();

    let items: Vec<(PageKind, &AnalyzedPage)> = analysis
        .entities
        .iter()
        .map(|p| (PageKind::Entity, p))
        .chain(analysis.concepts.iter().map(|p| (PageKind::Concept, p)))
        .collect();

    for (kind, item) in items {
        let title = item.title.trim().to_string();
        if title.is_empty() {
            continue; // 无名条目无法成为页面，丢弃
        }
        // 别名集：条目 aliases 去重并入 + title 自身始终在集内（不变量）。
        let mut aliases: Vec<String> = Vec::new();
        merge_aliases(&mut aliases, &item.aliases, &title);
        // 批内精确名去重：同名（title 或别名相撞）合并进已计划页面。
        if let Some(idx) = find_batch_match(&planned, &title, &aliases) {
            merge_into_planned(&mut planned[idx], item);
            continue;
        }

        // 与既有页面精确名去重（title/alias 任一相等，不区分大小写）。
        let matched = existing_index
            .iter()
            .find(|p| names_intersect(&p.owned_names(), &title, &aliases))
            .map(|p| (*p).clone());

        planned.push(PlannedPage {
            kind,
            title,
            aliases,
            tags: if item.tags.is_empty() {
                vec![kind.as_str().to_string()]
            } else {
                item.tags.clone()
            },
            related: item.related.clone(),
            summary: item.summary.clone(),
            existing: matched,
            path: String::new(),
        });
    }

    // Source page 的 related 指向本次全部产出页面（与正文产出索引呼应，
    // 让 Wiki Graph 从溯源锚点可达每个产出页）。
    if let Some((source, rest)) = planned.split_first_mut() {
        source.related = rest.iter().map(|p| p.title.clone()).collect();
    }

    // related 解析：本批页面名 → 该批 link；既有页面名 → 既有 link；
    // 解析不到的保留为短形式 unresolved wikilink（死链合法，目标页
    // 产出后 Obsidian 自动解析；死链检测属 T11 lint，本管道不裁决）。
    let link_by_batch_name: HashMap<String, String> = planned
        .iter()
        .flat_map(|p| {
            let link = format!("{}/{}", p.kind.dir(), slugify(&p.title));
            std::iter::once((normalize_name(&p.title), link.clone())).chain(
                p.aliases
                    .iter()
                    .map(move |a| (normalize_name(a), link.clone())),
            )
        })
        .collect();
    let link_by_existing_name: HashMap<String, String> = existing
        .iter()
        .flat_map(|p| {
            let link = p.path.strip_suffix(".md").unwrap_or(&p.path).to_string();
            std::iter::once((normalize_name(&p.frontmatter.title), link.clone())).chain(
                p.frontmatter
                    .aliases
                    .iter()
                    .map(move |a| (normalize_name(a), link.clone())),
            )
        })
        .collect();
    for page in &mut planned {
        if page.related.is_empty() {
            continue;
        }
        let related = std::mem::take(&mut page.related);
        page.related = related
            .iter()
            .map(|name| {
                let key = normalize_name(name);
                link_by_batch_name
                    .get(&key)
                    .or_else(|| link_by_existing_name.get(&key))
                    .map(|link| format!("[[{link}]]"))
                    .unwrap_or_else(|| format!("[[{name}]]"))
            })
            .collect();
    }

    // 路径分配：更新保留原路径；新建按 slug（同目录冲突追加序号）。
    let mut used_paths: HashMap<String, usize> = HashMap::new();
    for page in &mut planned {
        if let Some(existing_page) = &page.existing {
            page.path = existing_page.path.clone();
            continue;
        }
        let base = slugify(&page.title);
        let dir = page.kind.dir();
        let counter = used_paths.entry(format!("{dir}/{base}")).or_insert(0);
        let slug = if *counter == 0 {
            base.clone()
        } else {
            format!("{base}-{}", *counter + 1)
        };
        *counter += 1;
        page.path = format!("{dir}/{slug}.md");
    }
    Ok(planned)
}

/// 批内匹配：已计划页面的 title/aliases 与新条目名称相撞（不区分大小写）。
fn find_batch_match(planned: &[PlannedPage], title: &str, aliases: &[String]) -> Option<usize> {
    let new_names: Vec<String> = std::iter::once(normalize_name(title))
        .chain(aliases.iter().map(|n| normalize_name(n)))
        .collect();
    planned.iter().position(|p| {
        let owned: Vec<String> = std::iter::once(normalize_name(&p.title))
            .chain(p.aliases.iter().map(|n| normalize_name(n)))
            .collect();
        owned.iter().any(|o| new_names.contains(o))
    })
}

/// 批内同名的后续条目合并进首个计划页面（aliases/tags/related/summary 累积）。
fn merge_into_planned(target: &mut PlannedPage, item: &AnalyzedPage) {
    merge_aliases(&mut target.aliases, &item.aliases, &target.title);
    for tag in &item.tags {
        if !target.tags.contains(tag) {
            target.tags.push(tag.clone());
        }
    }
    target.related.extend(item.related.iter().cloned());
    if !item.summary.trim().is_empty() {
        target.summary = format!("{}\n\n{}", target.summary, item.summary);
    }
}

/// 名称集合相交判定（精确匹配、不区分大小写）。
fn names_intersect(owned: &[String], title: &str, aliases: &[String]) -> bool {
    let incoming: Vec<String> = std::iter::once(normalize_name(title))
        .chain(aliases.iter().map(|n| normalize_name(n)))
        .collect();
    owned.iter().any(|o| incoming.contains(o))
}

// ---------------------------------------------------------------------------
// 第二步：成文（strong 档）
// ---------------------------------------------------------------------------

/// 第二步 system 指令。
const COMPOSE_SYSTEM: &str = "\
You are a wiki page writer for an Obsidian-style knowledge vault. \
For every page in the given plan, write a Markdown body: concise, durable, \
knowledge-page oriented (not a chat reply). Follow the language of the raw \
source (default to Chinese when unclear). Reference other pages from the plan \
with [[wikilinks]] using exactly the paths given in the plan. The source page \
(sources/…) body starts with a summary; do NOT list produced pages there \
(the engine appends the index). Output contract — ONE JSON object and \
nothing else: {\"pages\": [{\"path\": \"<path from plan, without .md>\", \
\"body\": \"<markdown>\"}]}.";

/// 第二步调用：一次生成全部页面正文；解析失败 / 超限重试一次，再失败
/// 报错。`on_chars` 收到节流后的流式 chars 上报。
async fn compose(
    strong: &dyn LlmPort,
    template: &WikiTemplate,
    raw: &str,
    raw_rel: &str,
    planned: &[PlannedPage],
    recorder: &mut LlmCallRecorder,
    on_chars: &mut (dyn FnMut(usize) + Send),
) -> Result<HashMap<String, String>> {
    let plan_json: Vec<serde_json::Value> = planned
        .iter()
        .map(|p| {
            let mut item = serde_json::json!({
                "path": p.path.strip_suffix(".md").unwrap_or(&p.path),
                "type": p.kind.as_str(),
                "title": p.title,
                "aliases": p.aliases,
                "summary": p.summary,
                "related_links": p.related,
            });
            if let Some(existing) = &p.existing {
                item["existing_body"] =
                    serde_json::Value::String(truncate_chars(&existing.body, 2000));
            }
            item
        })
        .collect();
    let user = format!(
        "# Vault template\n{}\n\n# Page plan\n```json\n{}\n```\n\n# Raw source ({raw_rel})\n```text\n{raw}\n```\n",
        template.schema_prompt,
        serde_json::to_string_pretty(&plan_json)?,
    );

    let written = with_retry_stream(
        recorder,
        strong,
        Some(COMPOSE_SYSTEM),
        &user,
        CallProfile {
            tier: LlmTier::Strong,
            purpose: LlmCallPurpose::Compose,
            what: "成文",
            max_output_chars: COMPOSE_MAX_OUTPUT_CHARS,
        },
        parse_written,
        on_chars,
    )
    .await?;
    let mut bodies = HashMap::new();
    for page in written.pages {
        let key = page
            .path
            .strip_suffix(".md")
            .unwrap_or(&page.path)
            .to_string();
        bodies.insert(key, page.body);
    }
    Ok(bodies)
}

fn parse_written(output: &str) -> Result<WrittenPages> {
    let stripped = strip_think_blocks(output);
    let json = extract_json_object(&stripped);
    serde_json::from_str(json).map_err(|err| Error::BadLlmOutput(format!("JSON 解析失败：{err}")))
}

/// 按字符数截断（Unicode 安全；喂给 LLM 的既有正文限长用）。
fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let mut truncated: String = text.chars().take(max_chars).collect();
        truncated.push('…');
        truncated
    }
}

// ---------------------------------------------------------------------------
// Draft 组装
// ---------------------------------------------------------------------------

/// 把计划 + 正文组装成 Draft：frontmatter 合并（update 保留 created、
/// 并集 aliases/tags/related/sources）、Source 正文追加产出索引。
/// 第二步漏产出的页面以摘要兜底并记入 `degraded_paths`（显式降级标记）。
fn build_draft(
    raw_rel: &str,
    plan: Vec<PlannedPage>,
    bodies: &HashMap<String, String>,
    fingerprint: &str,
    llm_calls: Vec<LlmCallStats>,
) -> IngestDraft {
    let now = chrono::Local::now().to_rfc3339();
    let source_link = plan
        .first()
        .map(|p| p.path.strip_suffix(".md").unwrap_or(&p.path).to_string());

    let mut pages: Vec<PageDraft> = Vec::new();
    let mut degraded_paths: Vec<String> = Vec::new();
    for item in plan.iter().skip(1) {
        let (page, degraded) = build_page_draft(item, &source_link, bodies, &now, None);
        if degraded {
            degraded_paths.push(page.path.clone());
        }
        pages.push(page);
    }

    // Source page 放首位；正文尾部追加确定性产出索引（引擎生成非 LLM）。
    let source_item = plan.first().expect("页面计划至少含 Source 页");
    let (mut source, _) = build_page_draft(
        source_item,
        &source_link,
        bodies,
        &now,
        Some(RawPointer {
            raw_path: raw_rel,
            fingerprint,
        }),
    );
    source
        .body
        .push_str(&produced_index(&pages_meta(&pages, &degraded_paths)));
    let source_title = source.frontmatter.title.clone();
    pages.insert(0, source);

    IngestDraft {
        source_title,
        raw_path: raw_rel.to_string(),
        fingerprint: fingerprint.to_string(),
        pages,
        degraded_paths,
        llm_calls,
    }
}

/// 单个计划页面 → Draft 页面（附带「是否降级」：第二步漏产出该页正文）。
///
/// `raw_pointer` 仅 Source page 使用（raw 相对路径 + 指纹）。
fn build_page_draft(
    item: &PlannedPage,
    source_link: &Option<String>,
    bodies: &HashMap<String, String>,
    now: &str,
    raw_pointer: Option<RawPointer<'_>>,
) -> (PageDraft, bool) {
    let action = if item.existing.is_some() {
        PageAction::Update
    } else {
        PageAction::Create
    };
    let mut aliases = item.aliases.clone();
    let mut tags = item.tags.clone();
    let mut related = item.related.clone();
    let (path, created, mut sources) = match &item.existing {
        Some(old) => {
            // 更新：保留 created 与 path；字段取并集（旧在前，保序去重）。
            merge_aliases(&mut aliases, &old.frontmatter.aliases, &item.title);
            for tag in &old.frontmatter.tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }
            for r in &old.frontmatter.related {
                if !related.contains(r) {
                    related.push(r.clone());
                }
            }
            (
                old.path.clone(),
                old.frontmatter.created.clone(),
                old.frontmatter.sources.clone(),
            )
        }
        None => (item.path.clone(), now.to_string(), Vec::new()),
    };

    // sources：Entity/Concept 指回本次 Source page；Source page 自身为空
    //（它的原文指针是专有字段 raw_path + fingerprint）。
    if item.kind != PageKind::Source {
        if let Some(link) = source_link {
            let link_ref = format!("[[{link}]]");
            if !sources.contains(&link_ref) {
                sources.push(link_ref);
            }
        }
    }

    // aliases 非空不变量兜底（title 必在别名集内，空集回退 title）。
    aliases.retain(|a| !a.trim().is_empty());
    merge_aliases(&mut aliases, &[], &item.title);

    let (raw_path, fingerprint) = match raw_pointer {
        Some(pointer) => (
            Some(pointer.raw_path.to_string()),
            Some(pointer.fingerprint.to_string()),
        ),
        None => (None, None),
    };

    let frontmatter = PageFrontmatter {
        page_type: item.kind,
        title: item.title.clone(),
        aliases,
        tags,
        related,
        sources,
        created,
        updated: now.to_string(),
        raw_path,
        fingerprint,
    };

    let link = path.strip_suffix(".md").unwrap_or(&path).to_string();
    let degraded = !bodies.contains_key(&link);
    let body = match bodies.get(&link) {
        Some(body) => body.clone(),
        None => fallback_body(item),
    };
    (
        PageDraft {
            path,
            action,
            frontmatter,
            body: body.trim_end().to_string(),
        },
        degraded,
    )
}

/// 第二步漏掉某页时的兜底正文（管道继续，不阻断 Apply；降级事实另经
/// [`DEGRADED_NOTE`] 在产出索引与 Draft 检视清单中显式标注）。
fn fallback_body(item: &PlannedPage) -> String {
    format!(
        "（本页正文暂缺，以分析摘要代替）\n\n## 摘要\n\n{}",
        item.summary.trim()
    )
}

/// Source page 正文末尾的产出页面索引（确定性，引擎生成而非 LLM）；
/// 降级页条目带 [`DEGRADED_NOTE`] 标注。
fn produced_index(entries: &[(String, String, PageAction, bool)]) -> String {
    let mut out = String::from("\n\n## 产出页面\n\n");
    for (link, title, action, degraded) in entries {
        let verb = match action {
            PageAction::Create => "新建",
            PageAction::Update => "更新",
        };
        let mut note = format!("（{verb}）");
        if *degraded {
            note.push_str(DEGRADED_NOTE);
        }
        out.push_str(&format!("- {}{note}\n", wikilink(link, title)));
    }
    out
}

fn pages_meta(
    pages: &[PageDraft],
    degraded_paths: &[String],
) -> Vec<(String, String, PageAction, bool)> {
    pages
        .iter()
        .map(|p| {
            (
                p.link().to_string(),
                p.frontmatter.title.clone(),
                p.action,
                degraded_paths.contains(&p.path),
            )
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Apply 事务：原子写 + git 提交 + log/index 维护
// ---------------------------------------------------------------------------

/// 写前快照：`None` 表示文件原本不存在（恢复时删除）。
#[derive(Debug)]
struct FileSnapshot {
    path: PathBuf,
    old_content: Option<String>,
}

fn snapshot(path: PathBuf) -> FileSnapshot {
    let old_content = fs::read_to_string(&path).ok();
    FileSnapshot { path, old_content }
}

/// 恢复快照：写回旧内容或删除新建文件（best-effort，失败追加到错误消息）。
fn restore_all(snapshots: &[FileSnapshot]) {
    for snap in snapshots {
        let result = match &snap.old_content {
            Some(content) => atomic_write(&snap.path, content),
            None => match fs::remove_file(&snap.path) {
                Ok(()) => Ok(()),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(err) => Err(err.into()),
            },
        };
        if let Err(err) = result {
            eprintln!(
                "Apply 失败后的快照恢复出错（{}：{err}），请用 git 检查工作区",
                snap.path.display()
            );
        }
    }
}

/// index.md 增量更新（纯函数，便于单测）：首次清掉「（暂无页面）」占位，
/// 已有条目按 link 原位替换，新条目按提交顺序追加。
fn update_index(existing: &str, entries: &[(String, String)]) -> String {
    let mut lines: Vec<String> = existing
        .lines()
        .filter(|line| line.trim() != "（暂无页面）")
        .map(str::to_string)
        .collect();
    for (link, title) in entries {
        let prefix = format!("- [[{link}]]");
        let prefix_alias = format!("- [[{link}|");
        let rendered = format!("- {}", wikilink(link, title));
        match lines
            .iter()
            .position(|line| line.starts_with(&prefix) || line.starts_with(&prefix_alias))
        {
            Some(idx) => lines[idx] = rendered,
            None => lines.push(rendered),
        }
    }
    let mut out = lines.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// log.md 追加一条可 grep 的时序记录：`## [YYYY-MM-DD] ingest | <标题>`
/// （规格格式；git 提交信息另带 `corpus:` 前缀，见 [`INGEST_COMMIT_PREFIX`]）。
fn append_log(existing: &str, title: &str) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d");
    let mut out = existing.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("## [{today}] ingest | {title}\n"));
    out
}

/// 写入 + 提交（apply 的私有实现）：全部页面 tempfile persist 原子写 +
/// log/index 更新 + 恰好一个 git 提交。任一步失败恢复快照并清空暂存区
/// （commit 前的 `git add` 可能把文件写进了 index），不留半写状态。
fn write_and_commit(vault_root: &Path, draft: &IngestDraft) -> Result<String> {
    let wiki = vault_root.join("wiki");

    // 先在内存生成全部新内容：render 失败（schema 违规）时零写入。
    let mut writes: Vec<(PathBuf, String)> = Vec::new();
    for page in &draft.pages {
        writes.push((wiki.join(&page.path), page.render()?));
    }
    let index_entries: Vec<(String, String)> = draft
        .pages
        .iter()
        .map(|p| (p.link().to_string(), p.frontmatter.title.clone()))
        .collect();
    let index_path = wiki.join("index.md");
    let index_new = update_index(
        &fs::read_to_string(&index_path).unwrap_or_default(),
        &index_entries,
    );
    let log_path = wiki.join("log.md");
    let log_new = append_log(
        &fs::read_to_string(&log_path).unwrap_or_default(),
        &draft.source_title,
    );
    writes.push((index_path, index_new));
    writes.push((log_path, log_new));

    // 快照 → 建目录 → 逐文件原子写 → git add 指定文件 + 单次提交。
    let snapshots: Vec<FileSnapshot> = writes
        .iter()
        .map(|(path, _)| snapshot(path.clone()))
        .collect();
    for (path, content) in &writes {
        let result = (|| -> Result<()> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?; // wiki/sources 等子目录骨架未建
            }
            atomic_write(path, content)
        })();
        if let Err(err) = result {
            restore_all(&snapshots);
            return Err(err);
        }
    }
    let files: Vec<String> = draft
        .pages
        .iter()
        .map(|p| format!("wiki/{}", p.path))
        .chain([
            "wiki/index.md".to_string(),
            "wiki/log.md".to_string(),
            // raw 随本次提交入库：Source page 的指纹指针需要原文可溯。
            draft.raw_path.clone(),
        ])
        .collect();
    let message = format!("{INGEST_COMMIT_PREFIX} | {}", draft.source_title);
    match git::commit_files(vault_root, &files, &message) {
        Ok(commit) => Ok(commit),
        Err(err) => {
            restore_all(&snapshots);
            // commit 失败时 add 多半已把文件写入暂存区：mixed reset 清空
            // 之，否则索引里残留「已暂存但工作区已恢复」的幽灵条目，
            // 工作区并非真正回到提交点。
            if let Err(reset_err) = git::reset_index(vault_root) {
                eprintln!("git reset 失败（{reset_err}），请用 git status 检查暂存区");
            }
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_object_tolerates_fences_and_prose() {
        assert_eq!(
            extract_json_object(r#"```json {"a": 1} ```"#),
            r#"{"a": 1}"#
        );
        assert_eq!(
            extract_json_object("好的，结果如下：\n{\"a\": {\"b\": 2}}\n以上。"),
            r#"{"a": {"b": 2}}"#
        );
        assert_eq!(extract_json_object("完全不是 JSON"), "");
    }

    #[test]
    fn parse_analysis_accepts_valid_and_rejects_garbage() {
        let ok = parse_analysis(r#"{"source_title":"T","entities":[{"title":"E","aliases":["e"],"summary":"s"}],"concepts":[]}"#)
            .expect("合法输出应通过");
        assert_eq!(ok.source_title, "T");
        assert_eq!(ok.entities.len(), 1);
        assert_eq!(ok.entities[0].title, "E");
        // 缺字段走 serde default：不报错
        let partial = parse_analysis("{}").expect("空对象合法（全默认值）");
        assert!(partial.entities.is_empty());
        assert!(parse_analysis("垃圾输出").is_err());
    }

    #[test]
    fn strip_think_blocks_removes_closed_unclosed_and_multiple() {
        // 无思考块：零拷贝借用，原样返回
        assert!(matches!(
            strip_think_blocks(r#"{"a":1}"#),
            Cow::Borrowed(r#"{"a":1}"#)
        ));
        // 闭合块（含多行）：整段移除，正文保留
        assert_eq!(
            &strip_think_blocks("<think>\n多行\n思考\n</think>\n{\"a\":1}"),
            "\n{\"a\":1}"
        );
        // 多对块：逐一非贪婪移除
        assert_eq!(
            &strip_think_blocks("<think>A</think>X<think>B</think>Y"),
            "XY"
        );
        // 未闭合：标签开头到末尾整段视为思考，其后无正文
        assert_eq!(&strip_think_blocks("{\"a\":1}<think>截断的"), "{\"a\":1}");
        assert_eq!(&strip_think_blocks("<think>只有思考"), "");
        // 思考里含花括号也不污染：剥离后再提取（不剥离时 first-{ 落在
        // 思考块内，提取窗口会带上 `</think>` 导致解析失败）
        let polluted = "<think>思考里有个 { 假对象</think>{\"source_title\":\"T\"}";
        assert!(parse_analysis(polluted).is_ok(), "思考块应先剥离再提取");
    }

    #[test]
    fn failure_excerpt_is_single_lined_and_head_tailed() {
        let output = format!("{{\"a\":\"{}\",\"b\"", "\n".repeat(90));
        let excerpt = error_with_excerpt("原因", &output);
        assert!(excerpt.starts_with("原因；开头："));
        assert!(excerpt.contains("…结尾："));
        assert!(
            !excerpt.contains('\n'),
            "摘录应单行化（换行转空格）：{excerpt}"
        );
        // 90 个换行 + 头尾骨架 = 单行化后 head 80 空格、tail 80 空格
        let tail = single_line(&tail_chars(&output, ERROR_EXCERPT_CHARS));
        assert_eq!(tail.chars().count(), ERROR_EXCERPT_CHARS);
        assert_eq!(head_chars("中文abc", 2), "中文");
        assert_eq!(tail_chars("abcde", 2), "de");
        assert_eq!(tail_chars("ab", 5), "ab");
    }

    #[test]
    fn update_index_replaces_placeholder_then_upserts_lines() {
        let initial = "# Wiki Index\n\n说明。\n\n## 页面\n\n（暂无页面）\n";
        let v1 = update_index(initial, &[("entities/a".into(), "A".into())]);
        assert!(!v1.contains("（暂无页面）"), "占位行应被清除");
        assert!(v1.contains("- [[entities/a|A]]"));

        // 同 link 更新标题 → 原位替换不重复追加
        let v2 = update_index(&v1, &[("entities/a".into(), "A2".into())]);
        assert_eq!(v2.matches("[[entities/a").count(), 1);
        assert!(v2.contains("- [[entities/a|A2]]"));
        assert!(!v2.contains("A]]"));

        // 新条目追加（link != title 时带别名形式）
        let v3 = update_index(&v2, &[("sources/s".into(), "s".into())]);
        assert!(v3.contains("- [[sources/s|s]]"));
        // link == title 时用短形式
        let v4 = update_index(&v3, &[("concepts/c".into(), "concepts/c".into())]);
        assert!(v4.trim_end().ends_with("- [[concepts/c]]"));
    }

    #[test]
    fn append_log_adds_greppable_entry() {
        let initial = "# Wiki Log\n\n## [2026-01-01] init | Vault\n";
        let out = append_log(initial, "Attention Is All You Need");
        let last = out.lines().last().unwrap();
        assert!(
            last.starts_with("## [") && last.contains("] ingest | Attention Is All You Need"),
            "记录行应为 `## [日期] ingest | 标题`：{last}"
        );
        // 日期是今天（本地）
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        assert!(last.contains(&today));
    }

    #[test]
    fn produced_index_marks_degraded_pages() {
        let entries = vec![
            (
                "entities/a".to_string(),
                "A".to_string(),
                PageAction::Create,
                false,
            ),
            (
                "concepts/b".to_string(),
                "B".to_string(),
                PageAction::Create,
                true,
            ),
        ];
        let out = produced_index(&entries);
        assert!(
            out.contains("- [[entities/a|A]]（新建）\n"),
            "普通条目：{out}"
        );
        assert!(
            out.contains(&format!("- [[concepts/b|B]]（新建）{DEGRADED_NOTE}\n")),
            "降级条目须带显式标注：{out}"
        );
    }

    #[test]
    fn merge_aliases_dedupes_case_insensitively_and_keeps_title() {
        // title 自身始终在集内（即使 incoming 为空）
        let mut aliases = Vec::new();
        merge_aliases(&mut aliases, &[], "Transformer");
        assert_eq!(aliases, vec!["Transformer".to_string()]);

        // 并入去重（不区分大小写、跳过空白项与 title 重复）
        merge_aliases(
            &mut aliases,
            &[
                "  ".to_string(),
                "transformer".to_string(),
                "Transformer 架构".to_string(),
                "Transformer 架构".to_string(),
            ],
            "Transformer",
        );
        assert_eq!(
            aliases,
            vec!["Transformer".to_string(), "Transformer 架构".to_string()]
        );
    }

    #[test]
    fn truncate_chars_is_unicode_safe() {
        assert_eq!(truncate_chars("短文", 10), "短文");
        let long = "知识库知识库知识库";
        let cut = truncate_chars(long, 4);
        assert_eq!(cut, "知识库知…");
    }
}
