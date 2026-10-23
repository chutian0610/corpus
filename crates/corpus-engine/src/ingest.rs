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
//!   （恰好一个提交）+ log.md 追加 + index.md 全量重建（T9 富行，见
//!   [`rebuild_index`]）+ 指纹记录；任一步失败恢复快照并清空暂存区，
//!   不留半写状态。
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
//! JSON 输出的三级容错（解析层）：
//! 1. 剥离思考块 + 围栏/杂文字容忍（见 [`extract_json_object`]）；
//! 2. 截断修复（salvage）：serde 报截断特征错误（EOF/trailing/expected）
//!    时按括号平衡闭合恢复可解析前缀，部分条目可用即采用（统计标
//!    `salvaged=true`，见 [`salvage_truncated_json`]）；
//! 3. 最低有效性闸门（#23）：解析/修复成功后 source_title、entities、
//!    concepts 全空视为违约——任何形状不对但含 `{}` 的 JSON（如成文
//!    响应错位）都不得静默变成「全空分析」（空标题落回文件名、空计划
//!    产出垃圾页），违约走既有重试并附强约束，二次仍空报错零写入。
//!
//! 全程每次 LLM 调用都记 [`LlmCallStats`]（档位/用途/字数/耗时/结果，
//! 无原文；失败时附输出首尾片段诊断），随 [`IngestReport`] 返回并即时
//! 追加到 Vault 的 `.corpus/llm-calls.log`（JSON 行，被 .gitignore 忽略）。
//! 另有 `.corpus/llm-debug.log`（输入/输出原文，仅失败自动记录；设
//! `CORPUS_DEBUG=1` 时成功调用也全量记录，超 5MB 轮转保留一代）——
//! 该文件在用户本机 Vault 内、git 忽略，仅本地诊断用。
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
use crate::config::{AppConfig, ApplyMode, ProviderKind};
use crate::error::{Error, Result};
use crate::fingerprint::{sha256_hex, StateStore};
use crate::git;
use crate::llm::{
    LlmCallOutcome, LlmCallPurpose, LlmCallStats, LlmPort, LlmTier, ANALYSIS_MAX_TOKENS,
    COMPOSE_MAX_TOKENS,
};
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

// ---------------------------------------------------------------------------
// 编排层：import → 带上限端口 → ingest → 按 apply_mode apply
// ---------------------------------------------------------------------------

/// manual 模式（apply_mode=manual）结果的注明：Draft 未落盘，完整的人审
/// 流程（检视 / 编辑 / 放行）属票据 T5。原为桌面壳私有常量，随编排下沉
/// 收敛进引擎（双壳共用同一文案）。
pub const MANUAL_REVIEW_NOTE: &str = "人审模式：Draft 未落盘，完整人审流程属 T5";

/// `ingest_file` 编排的响应：[`IngestReport`] 平铺（serde flatten，JSON
/// 字段与两段式 API 完全一致）+ 导入信息 + Apply 模式语义。
#[derive(Debug, Clone, Serialize)]
pub struct IngestFileReport {
    /// 管道报告（页面清单 / 指纹 / LLM 统计等，字段平铺进本类型）。
    #[serde(flatten)]
    pub report: IngestReport,
    /// 导入后的 raw 文件相对 Vault 路径（如 `raw/foo.md`，
    /// [`import_raw`] 的结果；管道实际摄取的就是它）。
    pub imported_raw_path: String,
    /// 本次是否新导入进 raw/（false = 已在库 / 同内容去重命中既有文件）。
    pub newly_imported: bool,
    /// Draft 是否已落盘（指纹跳过与 manual 模式为 false）。
    pub applied: bool,
    /// manual 模式：Draft 人读检视清单（每页一行，降级页带标注）。
    pub summary_lines: Vec<String>,
    /// manual 模式注明（见 [`MANUAL_REVIEW_NOTE`]）。
    pub note: Option<String>,
}

/// 摄取一个文档的完整编排（端口注入变体，公共 API）：把「选文件 → 导入
/// → 摄取 → 按 `apply_mode` 落盘」收敛为一次调用——
///
/// 1. [`import_raw`]：把任意位置的源文件导入（复制）进 Vault 的 raw/
///    （源文件不动；同名去重 / 后缀递增）；
/// 2. [`ingest`]：既有管道（指纹检查 → 两步 LLM → Draft）；管道契约
///    不变——只接受 raw/ 内路径，入口由第 1 步的导入段保证；
/// 3. 按全局配置 `apply_mode` 分支：`auto` 显式 [`apply`]（原子写 +
///    恰好一个 git 提交 + 指纹记录）；`manual` 不落盘不提交，返回
///    Draft 的 [`IngestDraft::summary_lines`] 并注明
///    [`MANUAL_REVIEW_NOTE`]。
///
/// 端口注入测试缝：`fast` / `strong` 由调用方构造——真实路径（
/// [`ingest_file`]，rig 端口按档位带上限）与测试路径（Scripted Fake）
/// 共用本函数的全部编排逻辑，行为一致可离线验证；引擎在
/// `--no-default-features`（无 rig）下编排层依然可编译可测。
///
/// 传给 [`ingest`] 的是 `raw_rel`（相对 Vault 的导入路径）而非绝对路径：
/// 绝对路径经 import_raw canonicalize 后可能与未 canonicalize 的
/// `vault_root` 前缀不匹配（macOS tempdir 的 `/var` → `/private/var`），
/// 相对路径交由 [`ingest`] 按 `vault_root` 解析，无此坑。
pub async fn ingest_file_with_ports<F>(
    vault_root: impl AsRef<Path>,
    src_path: impl AsRef<Path>,
    config: &AppConfig,
    fast: &dyn LlmPort,
    strong: &dyn LlmPort,
    mut on_progress: F,
) -> Result<IngestFileReport>
where
    F: FnMut(IngestStage) + Send,
{
    let vault_root = vault_root.as_ref();
    // 第一段：导入（任意位置 → raw/；已在库则原样通过）。
    let imported = crate::import_raw(vault_root, src_path)?;

    // 第二段：既有 Ingest 管道（只接受 raw/ 内路径）。
    match ingest(
        vault_root,
        &imported.raw_rel,
        fast,
        strong,
        &mut on_progress,
    )
    .await?
    {
        IngestOutcome::Skipped(report) => Ok(IngestFileReport {
            report,
            imported_raw_path: imported.raw_rel,
            newly_imported: imported.imported,
            applied: false,
            summary_lines: Vec::new(),
            note: None,
        }),
        IngestOutcome::Draft(draft) => match config.apply_mode {
            ApplyMode::Auto => {
                let commit = apply(vault_root, &draft, on_progress)?;
                Ok(IngestFileReport {
                    report: draft.report(Some(commit)),
                    imported_raw_path: imported.raw_rel,
                    newly_imported: imported.imported,
                    applied: true,
                    summary_lines: Vec::new(),
                    note: None,
                })
            }
            // 人审模式：Draft 不落盘（零写入零提交），交还检视清单
            ApplyMode::Manual => Ok(IngestFileReport {
                report: draft.report(None),
                imported_raw_path: imported.raw_rel,
                newly_imported: imported.imported,
                applied: false,
                summary_lines: draft.summary_lines(),
                note: Some(MANUAL_REVIEW_NOTE.to_string()),
            }),
        },
    }
}

/// 摄取一个文档的完整编排（真实端口变体）：按全局配置的双模型档位构造
/// 两步端口（fast 分析 / strong 成文），并按档位预设服务端输出上限
/// （max_tokens：分析 [`ANALYSIS_MAX_TOKENS`] / 成文
/// [`COMPOSE_MAX_TOKENS`]，见 [`crate::build_llm_port_with_cap`]——
/// complete 协议无法按调用携带用途，按档位构造即等价于按用途设限），
/// 其余编排语义与 [`ingest_file_with_ports`] 完全一致；引擎侧另有字符
/// 熔断与调用统计（[`IngestReport::llm_calls`]，随报告透传）。
///
/// rig feature 之后（端口工厂属真实实现）；测试走
/// [`ingest_file_with_ports`] 的 Fake 注入。桌面壳与 corpus-server 均
/// 委托本函数，保证双壳行为一致。
#[cfg(feature = "rig")]
pub async fn ingest_file<F>(
    vault_root: impl AsRef<Path>,
    src_path: impl AsRef<Path>,
    config: &AppConfig,
    on_progress: F,
) -> Result<IngestFileReport>
where
    F: FnMut(IngestStage) + Send,
{
    // fast 档端口：MiniMax M3.1 系等推理模型强制思考（官方文档：
    // thinking.type=disabled 在 M3.1 上返回 400，只有旧 M3 可关；思考
    // token 计入 max_tokens 预算——max_tokens 太小会以 finish_reason=
    // length 结束且 content 为空），分析步改传 reasoning_effort=low
    // 大幅压低思考量（M3.1-Flash 专属参数，low–max，默认 max——
    // 默认 max 的超长思考正是分析输出被拦腰截断的元凶），
    // reasoning_split=true 确保思考分离进 reasoning_content、不混入
    // 正文流。strong 档保留思考（成文质量受益），仅设输出上限。
    let fast = if matches!(
        config.fast.provider,
        ProviderKind::OpenAICompatible | ProviderKind::Minimax
    ) {
        crate::rig_llm::build_llm_port_with(
            &config.fast,
            Some(ANALYSIS_MAX_TOKENS),
            Some(analysis_passthrough_params()),
        )?
    } else {
        crate::rig_llm::build_llm_port_with_cap(&config.fast, Some(ANALYSIS_MAX_TOKENS))?
    };
    let strong = crate::rig_llm::build_llm_port_with_cap(&config.strong, Some(COMPOSE_MAX_TOKENS))?;
    ingest_file_with_ports(vault_root, src_path, config, &fast, &strong, on_progress).await
}

/// 分析端口（OpenAICompatible / Minimax）的透传参数——`ingest_file` 原样并入
/// 请求体顶层（`AgentBuilder::additional_params`，provider passthrough）。
///
/// MiniMax 官方文档事实（2026-10 核对）：
/// - M3.1-Flash 强制思考：`thinking.type=disabled` 返回 400，只有旧 M3
///   可关——因此不传 `thinking`，改用思考量控制参数；
/// - `reasoning_effort`（M3.1-Flash 专属，low–max，默认 max）控制思考
///   token 量：传 `low` 大幅压低思考开销（思考 token 计入 `max_tokens`，
///   默认 max 的超长思考正是分析输出截断的元凶）；
/// - `reasoning_split: true` 把思考分离到 `reasoning_content`（思考与
///   正文分离；M3.1-Flash 不支持 false，缺省即分离——显式传 true 是
///   对其他 OpenAI 兼容后端的兼容声明，防止思考文本内联进 content）。
pub(crate) fn analysis_passthrough_params() -> serde_json::Value {
    serde_json::json!({ "reasoning_effort": "low", "reasoning_split": true })
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
pub(crate) const REPORT_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
/// ……或累计新增至少这么多字符（字符阈值），二者满足其一才上报。
pub(crate) const REPORT_MIN_NEW_CHARS: usize = 50;

/// 第一步（分析）的输出熔断上限（字符）。正常分析 JSON 应在几千字内，
/// 远小于此值；超过即视为模型复述原文 / 陷入重复，该次调用按失败处理。
/// 可按需调整（服务端 max_tokens 是第一道闸，这是引擎侧第二道）。
pub const ANALYSIS_MAX_OUTPUT_CHARS: usize = 30_000;

/// 第二步（成文）的输出熔断上限（字符）。可按需调整。
pub const COMPOSE_MAX_OUTPUT_CHARS: usize = 60_000;

/// 统计与日志文件的相对路径（Vault 内；被 `.corpus/.gitignore` 忽略）。
pub const LLM_CALLS_LOG_REL: &str = ".corpus/llm-calls.log";

/// LLM debug 日志的相对路径：记录每次调用的完整输入（system/user）与
/// 输出原文。**隐私说明**：该文件位于用户本机 Vault 的 `.corpus/` 内、
/// 被 `.corpus/.gitignore` 忽略（不入库），仅本地诊断用——排查「模型
/// 到底收到了什么、吐了什么」时全量可查，绝不随 git 历史/同步外流。
pub const LLM_DEBUG_LOG_REL: &str = ".corpus/llm-debug.log";

/// debug 日志轮转阈值（字节）：追加前超过即轮转为 `llm-debug.log.1`
///（保留一代，旧 `.1` 被覆盖）——原文全量记录的文件会长得很快。
pub const LLM_DEBUG_LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;

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

/// 上次输出解析成功但全空（最低有效性闸门违约，#23）时，重试 user
/// 消息末尾追加的强约束——明确点名必填字段，比「无法解析」更有针对性。
const RETRY_CONSTRAINT_EMPTY: &str = "上次输出缺少实质内容：source_title、entities、concepts \
不得全空。请严格按约定形状输出一个 JSON 对象，包含非空的 source_title、\
source_summary，以及 entities/concepts 列表（每个条目带非空 title）。";

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
    /// 构造（绑定 Vault，日志落在其 `.corpus/` 内）。
    pub(crate) fn new(vault_root: &Path) -> Self {
        Self {
            vault_root: vault_root.to_path_buf(),
            calls: Vec::new(),
        }
    }

    /// 记录一条统计：追加日志行（best-effort，失败只打 stderr 不阻断
    /// 摄取——诊断数据不能反噬主流程）+ 累积进内存（进报告）。
    pub(crate) fn record(&mut self, stats: LlmCallStats) {
        if let Err(err) = append_llm_call_line(&self.vault_root, &stats) {
            eprintln!("追加 LLM 调用日志失败（不影响摄取结果）：{err}");
        }
        self.calls.push(stats);
    }

    /// 收束为报告字段。
    fn finish(self) -> Vec<LlmCallStats> {
        self.calls
    }

    /// 追加一条 LLM debug 日志（完整输入/输出原文，见
    /// [`LLM_DEBUG_LOG_REL`]）：**失败调用自动记录**（端口错误 / 超限 /
    /// 解析或校验违约）；成功调用仅在 `CORPUS_DEBUG=1` 时全量记录。
    /// Best-effort：写日志失败只打 stderr，绝不影响摄取主流程。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn record_debug(
        &self,
        tier: LlmTier,
        purpose: LlmCallPurpose,
        outcome: LlmCallOutcome,
        system: Option<&str>,
        user: &str,
        output: &str,
        error: Option<&str>,
    ) {
        if outcome == LlmCallOutcome::Ok && !debug_always_enabled() {
            return;
        }
        let mut entry = serde_json::json!({
            "ts": chrono::Local::now().to_rfc3339(),
            "tier": tier,
            "purpose": purpose,
            "outcome": outcome,
            "system": system.unwrap_or(""),
            "user": user,
            "output": output,
        });
        if let Some(error) = error {
            entry["error"] = serde_json::Value::String(error.to_string());
        }
        if let Err(err) = append_llm_debug_line(&self.vault_root, &entry) {
            eprintln!("追加 LLM debug 日志失败（不影响摄取结果）：{err}");
        }
    }
}

/// `CORPUS_DEBUG=1`（精确值）：成功调用的输入/输出也全量写 debug 日志。
/// 失败调用无论该开关如何都会记录（诊断截断 / 违约问题的最小事实）。
fn debug_always_enabled() -> bool {
    std::env::var("CORPUS_DEBUG").is_ok_and(|v| v.trim() == "1")
}

/// 追加一行 JSON 到 Vault 的 llm-debug.log；追加前超
/// [`LLM_DEBUG_LOG_MAX_BYTES`] 则轮转为 `llm-debug.log.1`（保留一代）。
fn append_llm_debug_line(vault_root: &Path, entry: &serde_json::Value) -> Result<()> {
    let path = vault_root.join(LLM_DEBUG_LOG_REL);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    rotate_if_oversized(&path, LLM_DEBUG_LOG_MAX_BYTES)?;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{}", serde_json::to_string(entry)?)?;
    Ok(())
}

/// 日志轮转：`path` 超过 `max_bytes` 即重命名为 `<path>.1`（覆盖旧一代）。
/// 文件不存在时是空操作；轮转失败仅清掉旧 `.1` 后尽力 rename（诊断数据
/// 不反噬主流程，调用方已按 best-effort 处理错误）。
fn rotate_if_oversized(path: &Path, max_bytes: u64) -> Result<()> {
    let oversized = fs::metadata(path)
        .map(|meta| meta.len() > max_bytes)
        .unwrap_or(false);
    if !oversized {
        return Ok(());
    }
    let mut rotated = path.as_os_str().to_os_string();
    rotated.push(".1");
    let rotated = PathBuf::from(rotated);
    let _ = fs::remove_file(&rotated);
    fs::rename(path, &rotated)?;
    Ok(())
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

/// LLM 流式调用 + 输出熔断 + 解析/校验的重试助手（两步共用）：
///
/// - **熔断**：聚合输出超过 `profile.max_output_chars` 即视为该次失败
///   （不进入 parse——超限内容即使是合法 JSON 也不采信），原因写明
///   「输出 N 字超过上限 M，疑似模型复述原文或陷入重复」；
/// - **解析与截断修复**：`parse` 返回 `(值, salvaged)`——解析层内部已
///   尝试截断修复（见 [`parse_json_lenient`]），salvaged=true 表示输出
///   是括号平衡闭合的修复产物（部分条目），统计标 `salvaged` 字段；
/// - **校验（最低有效性闸门，#23）**：`validate` 返回 `Some(强约束句)`
///   即语义违约（如全空分析），该次按失败处理走重试——违约句同时是
///   重试 user 末尾追加的强约束；
/// - **重试**：解析失败 / 违约 / 超限均重试一次，重试的 user 消息末尾
///   按上次失败原因追加强约束句（[`RETRY_CONSTRAINT_OVERSIZED`] /
///   [`RETRY_CONSTRAINT_UNPARSEABLE`] / 校验返回的约束句）；两次失败报
///   [`Error::BadLlmOutput`]（`what` 为步骤名，错误含两次输出字数）；
///   端口调用错误（网络 / 鉴权 / 远端）记录统计后原样透传，不重试
///   （与既有语义一致——重试只针对输出质量问题）；
/// - **统计**：每次尝试记一条 [`LlmCallStats`]（`recorder` 落日志 +
///   进报告）。解析失败 / 超限的记录额外附失败诊断（输出首尾片段 +
///   思考标签检测，见 [`fill_failure_diagnostics`]），错误摘要末尾附
///   开头/结尾摘录（[`error_with_excerpt`]）——截断点与思考混流一眼
///   可见；端口调用错误（无输出）不记片段；
/// - **debug 日志**：失败调用（含端口错误）自动把完整输入/输出原文落
///   [`LLM_DEBUG_LOG_REL`]；成功调用仅 `CORPUS_DEBUG=1` 时记录
///   （见 [`LlmCallRecorder::record_debug`]）。
///
/// 文本增量经 [`LlmPort::complete_stream`] 即时到达，`on_chars` 是节流后
/// 的 chars 上报（模型已输出字符数）：距上次上报 ≥[`REPORT_MIN_INTERVAL`]
/// 或累计新增 ≥[`REPORT_MIN_NEW_CHARS`] 才放行——连续小增量合并成一报，
/// 避免高频回调淹没进度通道。流结束后补一次终值（尾部增量可能未达阈值）。
/// 每次尝试独立计数（重试的输出与上次无关）。
#[allow(clippy::too_many_arguments)]
async fn with_retry_stream<T>(
    recorder: &mut LlmCallRecorder,
    port: &dyn LlmPort,
    system: Option<&str>,
    user: &str,
    profile: CallProfile,
    parse: impl Fn(&str) -> Result<(T, bool)>,
    validate: &(dyn Fn(&T) -> Option<&'static str> + Send + Sync),
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
                    salvaged: None,
                });
                // debug 日志：失败自动记录（无输出，错误原样）
                recorder.record_debug(
                    tier,
                    purpose,
                    LlmCallOutcome::Error,
                    system,
                    &attempt_user,
                    "",
                    Some(&err.to_string()),
                );
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
            salvaged: None,
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
            recorder.record_debug(
                tier,
                purpose,
                LlmCallOutcome::Oversized,
                system,
                &attempt_user,
                &output,
                Some(&reason),
            );
            last_error = reason;
            retry_note = RETRY_CONSTRAINT_OVERSIZED.to_string();
            continue;
        }
        match parse(&output) {
            Ok((parsed, salvaged)) => {
                // 最低有效性闸门（#23）：解析/修复成功但内容违约（如全空
                // 分析）不得静默采信——按失败处理走重试，违约句兼作强约束。
                if let Some(note) = validate(&parsed) {
                    let reason = note.to_string();
                    let mut record = stats(
                        LlmCallOutcome::ParseFailed,
                        Some(error_with_excerpt(&format!("违约：{reason}"), &output)),
                    );
                    fill_failure_diagnostics(&mut record, &output);
                    recorder.record(record);
                    recorder.record_debug(
                        tier,
                        purpose,
                        LlmCallOutcome::ParseFailed,
                        system,
                        &attempt_user,
                        &output,
                        Some(&reason),
                    );
                    last_error = reason;
                    retry_note = note.to_string();
                    continue;
                }
                let mut record = stats(LlmCallOutcome::Ok, None);
                if salvaged {
                    // 截断修复产物被采用：统计与日志显式标注
                    record.salvaged = Some(true);
                }
                recorder.record(record);
                recorder.record_debug(
                    tier,
                    purpose,
                    LlmCallOutcome::Ok,
                    system,
                    &attempt_user,
                    &output,
                    None,
                );
                if salvaged {
                    eprintln!(
                        "输出 JSON 中途截断，已按括号平衡修复并采用部分条目（salvaged=true）"
                    );
                }
                return Ok(parsed);
            }
            Err(err) => {
                recorder.record_debug(
                    tier,
                    purpose,
                    LlmCallOutcome::ParseFailed,
                    system,
                    &attempt_user,
                    &output,
                    Some(&err.to_string()),
                );
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
/// 解析失败 / 超限 / 全空违约重试一次，再失败报 [`Error::BadLlmOutput`]
///（零写入）。`on_chars` 收到节流后的流式 chars 上报。
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
        &is_valid_analysis,
        on_chars,
    )
    .await
}

/// 最低有效性闸门（#23）：`AnalysisOutcome` 全空（source_title 为空 AND
/// entities 空 AND concepts 空）即违约——`#[serde(default)]` 使任何形状
/// 不对但含 `{}` 的 JSON（如成文响应错位、模型跑题输出）都会「成功」
/// 解析为全空分析：空标题落回 raw 文件名、空计划产出降级正文垃圾页，
/// 用户看到「摄取成功但内容是垃圾」。返回 `Some(强约束句)` 交
/// [`with_retry_stream`] 走既有重试，二次仍空报错零写入。
fn is_valid_analysis(analysis: &AnalysisOutcome) -> Option<&'static str> {
    let empty_shell = analysis.source_title.trim().is_empty()
        && analysis.entities.is_empty()
        && analysis.concepts.is_empty();
    empty_shell.then_some(RETRY_CONSTRAINT_EMPTY)
}

/// 容错解析分析输出：先剥离思考块，再提取 JSON（截断时尝试修复）。
/// 返回（结果, salvaged）——salvaged=true 表示输出是括号平衡闭合的
/// 截断修复产物（部分条目），由 [`with_retry_stream`] 标注统计。
fn parse_analysis(output: &str) -> Result<(AnalysisOutcome, bool)> {
    let stripped = strip_think_blocks(output);
    parse_json_lenient(&stripped)
}

/// 提取输出中的 JSON 对象字面量（容忍 ```json 围栏与前后杂文字）。
fn extract_json_object(output: &str) -> &str {
    match (output.find('{'), output.rfind('}')) {
        (Some(start), Some(end)) if end > start => &output[start..=end],
        _ => "",
    }
}

/// 宽容 JSON 解析：先按既有提取窗口（[`extract_json_object`]）走 serde；
/// 失败且错误带截断特征（EOF / trailing / expected——思考 token 挤占
/// max_tokens 截断输出的典型报错）时，尝试括号平衡修复
///（[`salvage_truncated_json`]）再解析一次，成功即采用（salvaged=true）。
/// 修复不可行或修复产物仍不可解析 → 回传原始错误（走既有重试）。
fn parse_json_lenient<T: serde::de::DeserializeOwned>(stripped: &str) -> Result<(T, bool)> {
    let json = extract_json_object(stripped);
    match serde_json::from_str(json) {
        Ok(value) => Ok((value, false)),
        Err(err) => {
            let message = err.to_string();
            let parse_error = || Error::BadLlmOutput(format!("JSON 解析失败：{message}"));
            let truncated = message.contains("EOF")
                || message.contains("trailing")
                || message.contains("expected");
            if !truncated {
                return Err(parse_error());
            }
            let Some(repaired) = salvage_truncated_json(stripped) else {
                return Err(parse_error());
            };
            match serde_json::from_str::<T>(&repaired) {
                Ok(value) => Ok((value, true)),
                // 截断得比任何完整元素更早 / 修复产物类型违约：回原始错误
                Err(_) => Err(parse_error()),
            }
        }
    }
}

/// 截断 JSON 的括号平衡修复（salvage）：从首个 `{` 扫描到文本末尾
///（截断输出天然缺尾），跟踪字符串 / 转义状态与 `{} []` 栈，收集全部
/// 「完整元素」边界候选——每个 `,` 之前（当前容器的元素刚结束）、每个
/// `}` / `]` 闭合之后、每个**值**字符串闭合之后（key 字符串不算：其后
/// 必有 `:`，在 key 处截断补闭合必然不可解析）；字符串开着到达文本末尾
/// 说明正悬在字符串中间，不产生「全文闭合」候选。
///
/// 候选按数据量从多到少尝试（末尾完整前缀 > 边界倒序），返回第一个能
/// 独立解析为 JSON 值的「前缀 + 补齐闭合括号」；最外层对象闭合即完整
/// JSON（trailing characters 场景），直接自检后返回。全部候选不可解析
/// 或无任何候选（开头即断 / 括号错乱）→ `None`，交由调用方走既有重试。
fn salvage_truncated_json(text: &str) -> Option<String> {
    let start = text.find('{')?;
    let body = &text[start..];
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    // 「期待值」位：true 时遇到的字符串是值（key 位置为 false）。
    let mut expect_value = false;
    // 边界候选（截断字节位相对 body，该点的括号栈快照），按出现序。
    let mut cuts: Vec<(usize, Vec<char>)> = Vec::new();

    for (idx, ch) in body.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
                if expect_value {
                    cuts.push((idx + ch.len_utf8(), stack.clone()));
                }
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' | '[' => {
                stack.push(ch);
                expect_value = ch == '[';
            }
            '}' | ']' => {
                let expected = if ch == '}' { '{' } else { '[' };
                if stack.pop() != Some(expected) {
                    return None; // 括号错乱（如杂文字混入）：不猜，放弃
                }
                expect_value = false;
                if stack.is_empty() {
                    // 最外层已闭合：完整 JSON，其后即使还有文本也不属于它
                    let complete = &body[..idx + ch.len_utf8()];
                    return serde_json::from_str::<serde_json::Value>(complete)
                        .is_ok()
                        .then(|| complete.to_string());
                }
                cuts.push((idx + ch.len_utf8(), stack.clone()));
            }
            ',' => {
                cuts.push((idx, stack.clone()));
                expect_value = stack.last() == Some(&'[');
            }
            _ => {}
        }
    }

    // 组装并按数据量从多到少尝试：末尾完整前缀（覆盖截断恰落在数字 /
    // 闭合符之后等场景）→ 各边界倒序（数据多的优先）。
    let mut attempts: Vec<String> = Vec::new();
    if !in_string {
        attempts.push(closed_at(body, body.len(), &stack));
    }
    for (cut, open) in cuts.iter().rev() {
        attempts.push(closed_at(body, *cut, open));
    }
    attempts
        .into_iter()
        .find(|candidate| serde_json::from_str::<serde_json::Value>(candidate).is_ok())
}

/// `body[..cut]` + 按栈快照补齐的闭合括号序列。
fn closed_at(body: &str, cut: usize, open: &[char]) -> String {
    let mut repaired = String::with_capacity(cut + open.len());
    repaired.push_str(&body[..cut]);
    for bracket in open.iter().rev() {
        repaired.push(if *bracket == '{' { '}' } else { ']' });
    }
    repaired
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
pub(crate) fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

/// 别名并集的唯一实现（build_plan / 批内合并 / 更新并集 / 不变量兜底
/// 四处收敛于此）：把 `incoming` 去空白、去重后并入 `aliases`（跳过与
/// `title` 或既有项重复的别名，不区分大小写），并保证 `title` 自身在
/// 集合内——「aliases 非空不变量」由此兜底。
pub(crate) fn merge_aliases(aliases: &mut Vec<String>, incoming: &[String], title: &str) {
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
    //
    // 种子化：既有页面的路径先行登记（含 .md 后缀剥除后的键），使
    // **跨摄取**的 slug 撞车也追加序号而非静默覆盖——两篇同题文章、
    // 或不同名字 slugify 出同一路径（"TCP/IP" 与 "TCP IP"）时，第二篇
    // 落到 `xxx-2.md`，双方溯源互不覆写。批内冲突沿用同一计数器语义
    // （第二个新建页 = `-2`）。
    let mut used_paths: HashMap<String, usize> = HashMap::new();
    for page in existing {
        let key = page
            .path
            .strip_suffix(".md")
            .unwrap_or(page.path.as_str())
            .to_string();
        *used_paths.entry(key).or_insert(0) += 1;
    }
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
        // 成文无额外语义校验（漏页走摘要兜底降级，不属违约）
        &|_: &WrittenPages| None,
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

/// 成文输出解析：与 [`parse_analysis`] 同一容错路径（剥离思考块 →
/// 提取 / 截断修复），返回（结果, salvaged）。
fn parse_written(output: &str) -> Result<(WrittenPages, bool)> {
    let stripped = strip_think_blocks(output);
    parse_json_lenient(&stripped)
}

/// 按字符数截断（Unicode 安全；喂给 LLM 的既有正文限长用）。
pub(crate) fn truncate_chars(text: &str, max_chars: usize) -> String {
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

/// 引擎级写锁（模块级单例）：ingest apply 与 query 沉淀走**同一个**
/// 落盘事务核心（[`write_pages_and_commit`]，写的是同一个 git 仓库的
/// 页面 + index + log + 提交），并发执行会交织提交与快照恢复。互斥
/// 下沉在引擎侧后，锁只覆盖**写盘段**——两边的 LLM 调用阶段都不持锁
/// （查询不阻塞摄取，摄取的长时间综合也不阻塞查询的词法检索）。
///
/// std Mutex：写盘段是同步代码、不跨 `.await`，临界区毫秒级到秒级
///（git 提交），短暂阻塞 async 线程可接受；毒锁自动恢复（写盘 panic
/// 后快照已尽力恢复，锁不传染后续事务）。
static CORPUS_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// 持引擎写锁执行 `f`（[`write_pages_and_commit`] 专用；锁语义见
/// [`CORPUS_WRITE_LOCK`]）。
fn with_corpus_write_lock<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _guard = CORPUS_WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    f()
}

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

/// index.md 全量重建的页面行条目（T9 富行）。
pub(crate) struct IndexEntry {
    /// wikilink 目标（相对 `wiki/`、去 `.md`）。
    pub link: String,
    /// 页面标题。
    pub title: String,
    /// 摘要首句（已剥 wikilink 标记并截断，见 [`index_digest`]）。
    pub digest: String,
    /// 资料源计数（frontmatter `sources` 数组长度）。
    pub source_count: usize,
}

/// 富行渲染（纯函数）：`- [[link|title]] — 摘要 · N 源`。
///
/// link == title 用短形式 `[[link]]`；页面无 sources（如 Source page /
/// Query page）省略计数段——计数为 0 的「· 0 源」是噪音不是信息。
pub(crate) fn render_index_line(entry: &IndexEntry) -> String {
    let mut line = format!(
        "- {} — {}",
        wikilink(&entry.link, &entry.title),
        entry.digest
    );
    if entry.source_count > 0 {
        line.push_str(&format!(" · {} 源", entry.source_count));
    }
    line
}

/// 摘要首句（纯函数）：跳过 Markdown 标题行与空行，取首个非空行，
/// 剥 wikilink 标记（`[[a|b]]`→`b`、`[[a]]`→`a`），截断约 60 字——
/// 超长时**优先在句读边界截断**（60 字窗口内最后一个句号/问号/叹号/
/// 分号，全半角皆可；首句经 `lines()` 逐行取出故换行边界天然不出现），
/// 窗口内无边界才按字符硬截（60 字 + 省略号）。
///
/// 数据源说明：页面 frontmatter 没有 summary 字段（T3 schema 七必填
/// 之外无该字段），富行的摘要取正文首句——设计补充评论中
/// 「frontmatter 的 summary（或正文首句）」的后者。
pub(crate) fn index_digest(body: &str) -> String {
    const DIGEST_MAX_CHARS: usize = 60;
    let first = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .unwrap_or("");
    let plain = strip_wikilinks(first);
    if plain.chars().count() <= DIGEST_MAX_CHARS {
        return plain;
    }
    let window: Vec<char> = plain.chars().take(DIGEST_MAX_CHARS).collect();
    if let Some(cut) = window
        .iter()
        .rposition(|c| matches!(c, '。' | '！' | '？' | '；' | '.' | '!' | '?' | ';'))
    {
        return window[..cut + 1].iter().collect();
    }
    truncate_chars(&plain, DIGEST_MAX_CHARS)
}

/// 剥一段文本里的 wikilink 标记：`[[target|label]]` → `label`，
/// `[[target]]` → `target`（富行摘要里嵌链接会让截断落在括号中间，
/// 渲染出破碎链接——摘要只留纯文本）。扫描走 [`crate::page::scan_wikilinks`]
/// 单一解析实现（未闭合的 `[[` 按字面文本保留，不是链接）。
pub(crate) fn strip_wikilinks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut offset = 0usize;
    for hit in crate::page::scan_wikilinks(text) {
        out.push_str(&text[offset..hit.start]);
        out.push_str(&hit.label);
        offset = hit.end;
    }
    out.push_str(&text[offset..]);
    out
}

/// index.md 全量重建（T9 富行升级）：扫描 `wiki/` 下全部页面类型的
/// `.md` 文件（含 `queries/`），按类型顺序 + 路径升序生成富行，拼在
/// 固定头部（[`crate::vault::INDEX_MD_HEADER`]）之后。
///
/// 为什么是全量重建而非旧的行级 upsert：富行格式上线后，行级替换只能
/// 刷新本次摄取触碰到的页面，旧格式行会长期残留——全量重建让既有
/// Vault 的 index 在**下次落盘事务**（ingest apply / 查询沉淀）时一次
/// 刷新到位，不做迁移工具。单页解析失败（坏 frontmatter）跳过该页，
/// 不阻断重建；目录缺失视为空（全新 Vault）。
pub(crate) fn rebuild_index(vault_root: &Path) -> Result<String> {
    let wiki = vault_root.join("wiki");
    let mut entries: Vec<IndexEntry> = Vec::new();
    for kind in PageKind::ALL {
        let mut paths: Vec<PathBuf> = Vec::new();
        collect_md_files(&wiki.join(kind.dir()), &mut paths);
        paths.sort();
        for path in paths {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok((frontmatter, body)) = parse_page(&content) else {
                continue; // 坏页不进目录（与 list_pages 宽松哲学一致）
            };
            let link = path
                .strip_prefix(&wiki)
                .expect("wiki 下的页面必然能 strip wiki/ 前缀")
                .to_string_lossy()
                .replace('\\', "/");
            let link = link.strip_suffix(".md").unwrap_or(&link).to_string();
            entries.push(IndexEntry {
                link,
                title: frontmatter.title,
                digest: index_digest(&body),
                source_count: frontmatter.sources.len(),
            });
        }
    }
    let mut out = String::from(crate::vault::INDEX_MD_HEADER);
    for entry in &entries {
        out.push_str(&render_index_line(entry));
        out.push('\n');
    }
    Ok(out)
}

/// 递归收集 `dir` 下的 `.md` 文件（不读内容；目录不存在为空操作）。
pub(crate) fn collect_md_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_md_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("md")
            && !path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("._"))
        {
            out.push(path);
        }
    }
}

/// 追加一条完整时序记录（`entry` 自带末尾换行；ingest 与 query 沉淀共用）。
pub(crate) fn append_log_entry(existing: &str, entry: &str) -> String {
    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(entry);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// 写入 + 提交（apply 的私有实现）：委托通用事务核心
/// [`write_pages_and_commit`]，额外把 raw 文件随本次提交入库
///（Source page 的指纹指针需要原文可溯）。
fn write_and_commit(vault_root: &Path, draft: &IngestDraft) -> Result<String> {
    let message = format!("{INGEST_COMMIT_PREFIX} | {}", draft.source_title);
    let today = chrono::Local::now().format("%Y-%m-%d");
    let log_entry = format!("## [{today}] ingest | {}\n", draft.source_title);
    write_pages_and_commit(
        vault_root,
        &draft.pages,
        std::slice::from_ref(&draft.raw_path),
        &message,
        &log_entry,
    )
}

/// 通用落盘事务核心（ingest apply 与 query 沉淀共用，T9）：全部页面
/// tempfile persist 原子写 + **index.md 全量重建**（[`rebuild_index`]，
/// 页面落盘后执行，磁盘即全量事实源）+ log 追加 + 恰好一个 git 提交。
///
/// **引擎级写互斥**：本函数是全部写盘事务的唯一入口，整体持
/// [`CORPUS_WRITE_LOCK`]（见其文档）——调用方（ingest apply / query
/// 沉淀）的 LLM 调用阶段在锁外，写盘段在此串行。任一步失败恢复快照
/// 并清空暂存区（commit 前的 `git add` 可能把文件写进了 index），不
/// 留半写状态。`extra_git_files` 是除页面与 index/log 外要一并提交的
/// Vault 相对路径（ingest 传 raw 文件）。
pub(crate) fn write_pages_and_commit(
    vault_root: &Path,
    pages: &[PageDraft],
    extra_git_files: &[String],
    commit_message: &str,
    log_entry: &str,
) -> Result<String> {
    with_corpus_write_lock(|| {
        write_pages_and_commit_locked(
            vault_root,
            pages,
            extra_git_files,
            commit_message,
            log_entry,
        )
    })
}

/// [`write_pages_and_commit`] 的锁内实现（调用方必须已持
/// [`CORPUS_WRITE_LOCK`]）。
fn write_pages_and_commit_locked(
    vault_root: &Path,
    pages: &[PageDraft],
    extra_git_files: &[String],
    commit_message: &str,
    log_entry: &str,
) -> Result<String> {
    let wiki = vault_root.join("wiki");

    // 先在内存生成全部页面内容：render 失败（schema 违规）时零写入。
    let mut writes: Vec<(PathBuf, String)> = pages
        .iter()
        .map(|page| Ok((wiki.join(&page.path), page.render()?)))
        .collect::<Result<_>>()?;
    let log_path = wiki.join("log.md");
    let log_new = append_log_entry(
        &fs::read_to_string(&log_path).unwrap_or_default(),
        log_entry,
    );
    writes.push((log_path, log_new));

    // 快照 → 建目录 → 逐文件原子写（index 不在先写之列：全量重建要以
    // 落盘后的全量页面为源）→ 重建 index → git add 指定文件 + 单次提交。
    let index_path = wiki.join("index.md");
    let mut snapshot_paths: Vec<PathBuf> = writes.iter().map(|(path, _)| path.clone()).collect();
    snapshot_paths.push(index_path.clone());
    let snapshots: Vec<FileSnapshot> = snapshot_paths.into_iter().map(snapshot).collect();
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
    let index_new = match rebuild_index(vault_root) {
        Ok(content) => content,
        Err(err) => {
            restore_all(&snapshots);
            return Err(err);
        }
    };
    if let Err(err) = atomic_write(&index_path, &index_new) {
        restore_all(&snapshots);
        return Err(err);
    }

    let files: Vec<String> = pages
        .iter()
        .map(|p| format!("wiki/{}", p.path))
        .chain(extra_git_files.iter().cloned())
        .chain(["wiki/index.md".to_string(), "wiki/log.md".to_string()])
        .collect();
    match git::commit_files(vault_root, &files, commit_message) {
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
        let (ok, salvaged) = parse_analysis(
            r#"{"source_title":"T","entities":[{"title":"E","aliases":["e"],"summary":"s"}],"concepts":[]}"#,
        )
        .expect("合法输出应通过");
        assert!(!salvaged);
        assert_eq!(ok.source_title, "T");
        assert_eq!(ok.entities.len(), 1);
        assert_eq!(ok.entities[0].title, "E");
        // 缺字段走 serde default：不报错（有效性与否由 is_valid_analysis 裁决）
        let (partial, salvaged) = parse_analysis("{}").expect("空对象合法（全默认值）");
        assert!(!salvaged);
        assert!(partial.entities.is_empty());
        assert!(parse_analysis("垃圾输出").is_err());
    }

    #[test]
    fn is_valid_analysis_rejects_only_the_empty_shell() {
        // 全空（title 空白 + 两列表空）→ 违约
        let shell = AnalysisOutcome {
            source_title: "   ".to_string(),
            ..AnalysisOutcome::default()
        };
        assert_eq!(is_valid_analysis(&shell), Some(RETRY_CONSTRAINT_EMPTY));
        assert_eq!(
            is_valid_analysis(&AnalysisOutcome::default()),
            Some(RETRY_CONSTRAINT_EMPTY)
        );
        // 任一实质内容存在即有效
        let with_title = AnalysisOutcome {
            source_title: "T".to_string(),
            ..AnalysisOutcome::default()
        };
        assert_eq!(is_valid_analysis(&with_title), None);
        let with_entity = AnalysisOutcome {
            entities: vec![AnalyzedPage {
                title: "E".to_string(),
                ..AnalyzedPage::default()
            }],
            ..AnalysisOutcome::default()
        };
        assert_eq!(is_valid_analysis(&with_entity), None);
    }

    /// 截断修复（salvage）：括号平衡闭合的纯函数行为。
    #[test]
    fn salvage_truncated_json_closes_brackets_at_last_complete_element() {
        // 实体数组写到一半断掉：完整条目全部保留，悬在半空的字符串整体丢弃
        let truncated =
            r#"{"source_title":"T","entities":[{"title":"X","summary":"Y"},{"title":"Q","summ"#;
        let repaired = salvage_truncated_json(truncated).expect("应可修复");
        assert_eq!(
            repaired,
            r#"{"source_title":"T","entities":[{"title":"X","summary":"Y"},{"title":"Q"}]}"#
        );
        // 末尾完整（截在数字 / 闭合符之后）：全文闭合，一个不丢
        assert_eq!(
            salvage_truncated_json(r#"{"a":[1,2"#).as_deref(),
            Some(r#"{"a":[1,2]}"#)
        );
        assert_eq!(
            salvage_truncated_json(r#"{"a":[{"x":1"#).as_deref(),
            Some(r#"{"a":[{"x":1}]}"#)
        );
        // 字符串中途截断（值字符串悬空）：回退到最后一个完整元素
        assert_eq!(
            salvage_truncated_json(r#"{"source_title":"T","source_summary":"断"#).as_deref(),
            Some(r#"{"source_title":"T"}"#)
        );
        // key 字符串不是边界（其后必有冒号，在 key 处闭合不可解析）
        assert_eq!(
            salvage_truncated_json(r#"{"a":"v","b"#).as_deref(),
            Some(r#"{"a":"v"}"#)
        );
        // 完全没有完整元素：无法修复
        assert_eq!(salvage_truncated_json(r#"{"source_title"#), None);
        assert_eq!(salvage_truncated_json(""), None);
        // 字符串内的括号 / 引号转义不干扰扫描
        assert_eq!(
            salvage_truncated_json(r#"{"a":"他说\"{[","b":[1,2"#).as_deref(),
            Some(r#"{"a":"他说\"{[","b":[1,2]}"#)
        );
        // 已闭合的最外层对象（trailing characters 场景）：其后杂文字不算
        assert_eq!(
            salvage_truncated_json(r#"{"a":1} 以上是结果，{ 随便"#).as_deref(),
            Some(r#"{"a":1}"#)
        );
        // 修复产物必须真的可解析并产出部分条目
        let (parsed, salvaged) = parse_analysis(truncated).expect("截断输出应修复成功");
        assert!(salvaged, "截断修复应标 salvaged=true");
        assert_eq!(parsed.source_title, "T");
        assert_eq!(parsed.entities.len(), 2, "完整条目全部保留");
        assert_eq!(parsed.entities[0].title, "X");
        assert_eq!(parsed.entities[1].title, "Q");
    }

    /// 截断 JSON 端到端：entities 写到一半断掉 → salvage 恢复部分条目
    /// 而非走重试；成文输出同理。
    #[test]
    fn truncated_json_salvages_partial_entries_without_retry() {
        let truncated = r#"{"source_title":"Attention","source_summary":"s",
  "entities":[{"title":"Transformer","summary":"架构"},{"title":"BERT","summa"#;
        let (outcome, salvaged) = parse_analysis(truncated).expect("截断输出应修复成功");
        assert!(salvaged);
        assert_eq!(outcome.source_title, "Attention");
        assert_eq!(outcome.entities.len(), 2);
        assert_eq!(outcome.entities[0].title, "Transformer");
        assert_eq!(outcome.entities[1].title, "BERT");
        // 成文同样受益
        let (written, salvaged) =
            parse_written(r#"{"pages":[{"path":"sources/a","body":"b"},{"path":"entities/x""#)
                .expect("截断成文输出应修复成功");
        assert!(salvaged);
        assert_eq!(written.pages.len(), 2);
        assert_eq!(written.pages[0].path, "sources/a");
        assert_eq!(written.pages[1].path, "entities/x");
    }

    /// 完全损坏的输出（无任何完整元素 / 类型违约）：salvage 不可行 →
    /// 维持解析失败（由 with_retry_stream 走重试，集成测试覆盖调用次数）。
    #[test]
    fn fully_broken_json_still_fails_parse_for_retry() {
        assert!(parse_analysis(r#"{这不是 JSON，连括号都不齐"#).is_err());
        assert!(parse_analysis("垃圾输出").is_err());
        // 类型违约（形状合法但字段类型错）不走截断修复
        assert!(parse_analysis(r#"{"entities":"not-an-array"}"#).is_err());
    }

    /// debug 日志轮转：未超阈值不动；超阈值轮转为 `.1`（保留一代，
    /// 再次轮转覆盖旧一代）；轮转阈值为 5MB。
    #[test]
    fn debug_log_rotation_keeps_one_generation() {
        assert_eq!(LLM_DEBUG_LOG_MAX_BYTES, 5 * 1024 * 1024, "轮转上限应为 5MB");
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let path = dir.path().join("llm-debug.log");
        let rotated = dir.path().join("llm-debug.log.1");

        // 未超阈值：不轮转
        fs::write(&path, "当前一代内容").expect("写日志失败");
        rotate_if_oversized(&path, 1000).expect("轮转检查失败");
        assert!(path.exists());
        assert!(!rotated.exists());

        // 超阈值：轮转，旧文件成为 .1
        rotate_if_oversized(&path, 2).expect("轮转失败");
        assert!(!path.exists(), "原文件应已改名");
        assert_eq!(
            fs::read_to_string(&rotated).unwrap(),
            "当前一代内容",
            "旧一代保留在 .1"
        );

        // 再次轮转：覆盖旧一代（只保留一代）
        fs::write(&path, "新一代").expect("写日志失败");
        rotate_if_oversized(&path, 2).expect("二次轮转失败");
        assert_eq!(fs::read_to_string(&rotated).unwrap(), "新一代");

        // 文件不存在：空操作（二次轮转后原路径已随改名消失）
        assert!(!path.exists());
        rotate_if_oversized(&path, 2).expect("缺文件应空操作");
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
    fn render_index_line_rich_formats() {
        // 富行：wikilink — 摘要 · N 源；link == title 用短形式；
        // 无 sources 的页面省略计数段
        let with_sources = render_index_line(&IndexEntry {
            link: "entities/transformer".into(),
            title: "Transformer".into(),
            digest: "完全基于注意力的架构".into(),
            source_count: 2,
        });
        assert_eq!(
            with_sources,
            "- [[entities/transformer|Transformer]] — 完全基于注意力的架构 · 2 源"
        );
        let short_link = render_index_line(&IndexEntry {
            link: "自注意力".into(),
            title: "自注意力".into(),
            digest: "让每个位置直接关注全部位置".into(),
            source_count: 0,
        });
        assert_eq!(short_link, "- [[自注意力]] — 让每个位置直接关注全部位置");
    }

    #[test]
    fn index_digest_takes_first_plain_sentence_stripped_and_truncated() {
        // 跳过标题行与空行，取首个非空行；wikilink 剥成纯文本
        let body = "# Transformer\n\n[[entities/transformer|Transformer]] 是完全基于注意力的架构。\n\n第二段。";
        assert_eq!(index_digest(body), "Transformer 是完全基于注意力的架构。");
        // 超长截断：60 字窗口内有句读边界 → 在**最后一个**句号处截断
        //（保留完整句子，无省略号）
        let long = format!("第一句在这里结束。{}无边界填充", "字".repeat(80));
        let digest = index_digest(&format!("# 标题\n\n{long}"));
        assert_eq!(digest, "第一句在这里结束。", "应在句读边界截断：{digest}");
        // 窗口内无任何句读边界 → 硬截（60 字 + 省略号，既有行为）
        let boundless = "字".repeat(80);
        let digest = index_digest(&format!("# 标题\n\n{boundless}"));
        assert_eq!(digest.chars().count(), 61, "60 字 + 省略号：{digest}");
        assert!(digest.ends_with('…'));
        // 半角句读同样是边界
        let half = format!("Head sentence. {}", "x".repeat(80));
        let digest = index_digest(&half);
        assert_eq!(digest, "Head sentence.", "半角句号也是边界：{digest}");
        // 全是标题行：摘要在 index 行里是可辨识但空的兜底
        assert_eq!(index_digest("# 只有标题\n\n"), "");
        // 未闭合的 [[ 按字面保留
        assert_eq!(index_digest("悬空的 [[链接 与正文"), "悬空的 [[链接 与正文");
    }

    /// index 全量重建：含富行与占位清除语义；旧格式行随重建消失
    /// （既有 Vault 的迁移路径 = 下次落盘事务的全量重建，无迁移工具）。
    #[test]
    fn rebuild_index_regenerates_rich_lines_from_disk() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let root = dir.path().join("vault");
        crate::create_vault(&root, &crate::template::TemplateId::from("generic"))
            .expect("建 Vault 失败");
        let fm = |title: &str| {
            format!(
                "---\ntype: entity\ntitle: {title}\naliases: [{title}]\ntags: []\nrelated: []\nsources: [\"[[sources/a]]\"]\ncreated: t\nupdated: t\n---\n\n"
            )
        };
        let entities = root.join("wiki/entities");
        fs::create_dir_all(&entities).expect("建目录失败");
        fs::write(entities.join("a.md"), format!("{}旧格式正文", fm("A"))).expect("写失败");
        fs::write(
            entities.join("b.md"),
            format!("{}[[entities/a|A]] 引用与正文摘要句。", fm("B")),
        )
        .expect("写失败");

        let index = rebuild_index(&root).expect("重建失败");
        assert!(index.starts_with(crate::vault::INDEX_MD_HEADER));
        assert!(!index.contains("（暂无页面）"), "占位应被重建清除");
        // 富行：摘要 + 来源计数（sources 数组长 1）
        assert!(
            index.contains("- [[entities/a|A]] — 旧格式正文 · 1 源"),
            "富行应含摘要与源数：{index}"
        );
        assert!(
            index.contains("- [[entities/b|B]] — A 引用与正文摘要句。 · 1 源"),
            "wikilink 应剥成纯文本摘要：{index}"
        );
        // 路径升序稳定（a 在 b 前）
        let a_pos = index.find("[[entities/a|A]]").expect("缺 a 行");
        let b_pos = index.find("[[entities/b|B]]").expect("缺 b 行");
        assert!(a_pos < b_pos);

        // 坏 frontmatter 页面：跳过不阻断
        fs::write(
            entities.join("broken.md"),
            "---\ntitle: 缺 aliases\n---\n\n正文",
        )
        .expect("写失败");
        let index = rebuild_index(&root).expect("坏页不应阻断重建");
        assert!(!index.contains("broken"), "坏页不进目录：{index}");
    }

    #[test]
    fn append_log_entry_normalizes_trailing_newlines() {
        // 无尾换行 / 空文件 / 已有尾换行：都恰好追加一条带换行的记录
        assert_eq!(
            append_log_entry("# Wiki Log", "## [2026-01-01] ingest | A\n"),
            "# Wiki Log\n## [2026-01-01] ingest | A\n"
        );
        assert_eq!(
            append_log_entry("", "## [d] query | Q\n"),
            "## [d] query | Q\n"
        );
        assert_eq!(
            append_log_entry("# Log\n", "## [d] query | Q\n"),
            "# Log\n## [d] query | Q\n"
        );
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
