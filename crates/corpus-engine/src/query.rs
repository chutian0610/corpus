//! 自然语言查询（票据 T9，查询最小闭环）：词法候选 → LLM 综合 →
//! Query page 沉淀。
//!
//! 级联（CONTEXT.md「Query page」/ issue #9 设计补充）：
//! 1. [`search_pages`]：**确定性词法打分，零 LLM**——问题分词后对每页
//!    打分（title 命中权重高、aliases 命中、正文命中计数），取 TopN
//!    候选（[`SEARCH_TOP_N`]）。词法层是查询的第一级，独立可测
//!    （验收：候选选取不依赖 LLM）；
//! 2. 读候选页正文（每页截断至 [`QUERY_CONTEXT_PAGE_MAX_CHARS`]，防
//!    上下文爆炸）→ [`LlmPort::complete_stream`] 综合（system 指示：
//!    只基于给定页面回答、引用必须用候选页的 `[[path|title]]` 形式、
//!    不知道就说不知道），流式增量经节流上报 [`QueryEvent::Answering`]；
//! 3. 沉淀：`config.query_pages == true` 时经与 ingest apply 同一事务
//!    核心（[`crate::ingest::write_pages_and_commit`]：原子写 + index
//!    全量重建 + log 追加 + 恰好一个提交）落 Query page
//!    （type=query，title=问题截断，body=完整回答，related=候选页，
//!    aliases=[问题]）；false 时跳过落盘。
//!
//! 边界行为：无候选页 → 直接返回「知识库中没有相关页面」，**不调
//! LLM、不沉淀**（没有上下文的综合只会幻觉，落盘只会垃圾）；LLM 调用
//! 失败照常报错（错误不静默）。回答是自由文本，不走 ingest 的 JSON
//! 解析/重试（无解析层可言），端口错误即整单失败。
//!
//! 引擎不依赖 tauri：进度经 `on_event: impl FnMut(QueryEvent)` 回调
//! 上报，壳层负责转成事件（与 ingest 同模式）。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::AppConfig;
use crate::error::{Error, Result};
use crate::ingest::{
    merge_aliases, normalize_name, truncate_chars, write_pages_and_commit, REPORT_MIN_INTERVAL,
    REPORT_MIN_NEW_CHARS,
};
use crate::llm::{LlmCallOutcome, LlmCallPurpose, LlmCallStats, LlmPort, LlmTier};
use crate::page::{
    parse_page, slugify, wikilink, PageAction, PageDraft, PageFrontmatter, PageKind,
};

/// 查询提交信息前缀（`corpus: query | <问题截断>`，可 grep）。
pub const QUERY_COMMIT_PREFIX: &str = "corpus: query";

/// 词法候选 TopN（默认 6，常量可调）。
pub const SEARCH_TOP_N: usize = 6;

/// 注入 LLM 的单页正文上限（字符）：候选多页全量注入会撑爆上下文，
/// 截断保头部的信息密度足够词法命中场景使用。
pub const QUERY_CONTEXT_PAGE_MAX_CHARS: usize = 4000;

/// Query page 标题的最大字符数（title = 问题截断）。
pub const QUERY_TITLE_MAX_CHARS: usize = 30;

/// 无候选页时的固定回答（不调 LLM、不沉淀）。
pub const NO_CANDIDATE_ANSWER: &str = "知识库中没有相关页面。";

/// 查询综合 system 指令：只基于候选页回答、wikilink 引用、不知则说不知。
const ANSWER_SYSTEM_PROMPT: &str = "\
You are the knowledge assistant of an Obsidian-style wiki vault (Corpus). \
Answer the user's question using ONLY the candidate pages provided in the \
user message. Cite pages inline as wikilinks using exactly the candidate \
page paths given (e.g. [[entities/transformer|Transformer]]); never invent \
paths. If the candidate pages do not contain enough information, say so \
plainly (e.g. \"知识库中没有相关内容\") instead of guessing. Answer in the \
language of the question (default to Chinese).";

/// 查询进度事件（tag=stage 风格，与 IngestStage 同口径）：
/// `Searching` → `Answering`（流式增量，节流沿用 ingest 模式）→ `Done`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum QueryEvent {
    /// 词法候选检索开始，携带归一化后的全部词项（前端可展示）。
    Searching {
        /// 分词产物（去重排序；CJK 按字/二元组 + ASCII 按词）。
        terms: Vec<String>,
    },
    /// LLM 综合回答中：同阶段重复事件携带递增 `chars`（模型已输出
    /// 字符数，节流口径同 ingest），`chars: 0` 表示阶段开始。只报字数
    /// 不报内容——增量内容会在完成后随结果整体到达。
    Answering {
        /// 模型已输出的字符数（0 = 阶段开始）。
        chars: usize,
    },
    /// 查询完成（结果与沉淀信息在返回值 `QueryOutcome` 中）。
    Done,
}

/// 词法打分后的候选页（score 降序；确定性排序）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScoredPage {
    /// 相对 `wiki/` 的 `.md` 路径。
    pub path: String,
    /// 页面标题。
    pub title: String,
    /// 词法得分（仅排序用，前端可展示）。
    pub score: i64,
}

/// 回答中实际引用到的候选页。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryCitation {
    /// 相对 `wiki/` 的 `.md` 路径。
    pub path: String,
    /// 页面标题。
    pub title: String,
}

/// [`query_with_port`] 的结果。
#[derive(Debug, Clone, Serialize)]
pub struct QueryOutcome {
    /// 原始问题（trim 后）。
    pub question: String,
    /// 完整回答文本（可含 `[[wikilink]]`，前端复用 wiki-links 渲染）。
    pub answer: String,
    /// 词法候选页（TopN，score 降序）。
    pub candidates: Vec<ScoredPage>,
    /// 回答中实际引用到的候选页（按出现顺序去重）。
    pub citations: Vec<QueryCitation>,
    /// 沉淀的 Query page 相对 `wiki/` 路径（配置关闭或无候选时 `None`）。
    pub query_page_path: Option<String>,
    /// 沉淀产生的 git 短 hash（未沉淀时 `None`）。
    pub commit: Option<String>,
}

// ---------------------------------------------------------------------------
// 词法层：分词 + 打分 + 检索（确定性，零 LLM）
// ---------------------------------------------------------------------------

/// 分词词项上限：超长问题（粘贴整段文章）的词项数有界，Searching 事件
/// 与打分开销都不失控；截断只影响召回，不报错。
const MAX_TERMS: usize = 64;

/// 问题分词（确定性，实现从简）：ASCII 按字母数字词（小写化），CJK 连续
/// 段按**单字 + 相邻二元组**展开——单字保召回、二元组保精度；其余字符
/// （标点/空白）为分隔。后续可替换为 jieba 类真分词器，本函数是唯一
/// 接缝。产物去重排序（打分只看词项集合，与出现顺序无关）。
pub(crate) fn tokenize_question(question: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    let lower = question.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() && terms.len() < MAX_TERMS {
        let c = chars[i];
        if c.is_ascii_alphanumeric() {
            let start = i;
            while i < chars.len() && chars[i].is_ascii_alphanumeric() {
                i += 1;
            }
            terms.push(chars[start..i].iter().collect());
        } else if is_cjk(c) {
            let start = i;
            while i < chars.len() && is_cjk(chars[i]) {
                i += 1;
            }
            for (j, ch) in chars[start..i].iter().enumerate() {
                if terms.len() >= MAX_TERMS {
                    break;
                }
                terms.push(ch.to_string());
                if j + 1 < chars[start..i].len() && terms.len() < MAX_TERMS {
                    terms.push(format!("{}{}", ch, chars[start + j + 1]));
                }
            }
        } else {
            i += 1;
        }
    }
    terms.sort();
    terms.dedup();
    terms
}

/// CJK 统一表意文字判定（基本区 + 扩展 A；覆盖中文常用字，实现从简）。
fn is_cjk(c: char) -> bool {
    matches!(c, '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}')
}

/// 词法打分权重：title 精确含词（权重高）。
const TITLE_HIT_WEIGHT: i64 = 10;
/// 词法打分权重：aliases 命中。
const ALIAS_HIT_WEIGHT: i64 = 6;
/// 词法打分权重：正文每次命中 +1。
const BODY_HIT_WEIGHT: i64 = 1;
/// 单词项的正文命中计数封顶（长文堆次数不可无限刷分）。
const BODY_HIT_MAX_PER_TERM: usize = 5;

/// 对单页打分（确定性纯函数）：对每个词项，title 含词 +[`TITLE_HIT_WEIGHT`]、
/// 任一别名含词 +[`ALIAS_HIT_WEIGHT`]（各计一次，不区分大小写）、正文
/// 命中次数（封顶 [`BODY_HIT_MAX_PER_TERM`]）×[`BODY_HIT_WEIGHT`]。
fn score_page(terms: &[String], title: &str, aliases: &[String], body: &str) -> i64 {
    let title_norm = normalize_name(title);
    let alias_norms: Vec<String> = aliases.iter().map(|a| normalize_name(a)).collect();
    let body_norm = body.to_lowercase();
    let mut score = 0i64;
    for term in terms {
        if title_norm.contains(term.as_str()) {
            score += TITLE_HIT_WEIGHT;
        }
        if alias_norms.iter().any(|a| a.contains(term.as_str())) {
            score += ALIAS_HIT_WEIGHT;
        }
        let hits = body_norm
            .matches(term.as_str())
            .count()
            .min(BODY_HIT_MAX_PER_TERM);
        score += hits as i64 * BODY_HIT_WEIGHT;
    }
    score
}

/// 词法候选检索（公共 API，**确定性纯函数，零 LLM**）：分词 → 对
/// `wiki/` 下全部页面（含 queries/，先前问答也是 Wiki 的一部分）打分
/// → 得分 > 0 的页面按（score 降序，路径升序）排序取 [`SEARCH_TOP_N`]。
///
/// 单页解析失败宽松跳过（与 list_pages 同哲学）；非 Vault 报
/// [`Error::NotAVault`]；空白问题（分词为空）返回空候选。
pub fn search_pages(vault_root: impl AsRef<Path>, question: &str) -> Result<Vec<ScoredPage>> {
    let root = vault_root.as_ref();
    if !root.join("wiki").is_dir() {
        return Err(Error::NotAVault {
            path: root.to_path_buf(),
            reason: "缺 wiki/ 目录".to_string(),
        });
    }
    let terms = tokenize_question(question);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut scored: Vec<ScoredPage> = Vec::new();
    for page in scan_pages(root)? {
        let score = score_page(
            &terms,
            &page.frontmatter.title,
            &page.frontmatter.aliases,
            &page.body,
        );
        if score > 0 {
            scored.push(ScoredPage {
                path: page.path,
                title: page.frontmatter.title,
                score,
            });
        }
    }
    scored.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.cmp(&b.path)));
    scored.truncate(SEARCH_TOP_N);
    Ok(scored)
}

/// 检索扫描出的页面（frontmatter + 正文）。
struct ScannedPage {
    /// 相对 `wiki/` 的 `.md` 路径。
    path: String,
    frontmatter: PageFrontmatter,
    body: String,
}

/// 扫描 `wiki/` 下全部页面类型目录（含 queries/），宽松跳过坏页。
fn scan_pages(vault_root: &Path) -> Result<Vec<ScannedPage>> {
    let wiki = vault_root.join("wiki");
    let mut pages = Vec::new();
    for kind in PageKind::ALL {
        let mut paths: Vec<PathBuf> = Vec::new();
        crate::ingest::collect_md_files(&wiki.join(kind.dir()), &mut paths);
        for path in paths {
            let Ok(content) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok((frontmatter, body)) = parse_page(&content) else {
                continue;
            };
            pages.push(ScannedPage {
                path: path
                    .strip_prefix(&wiki)
                    .expect("wiki 下的页面必然能 strip wiki/ 前缀")
                    .to_string_lossy()
                    .replace('\\', "/"),
                frontmatter,
                body,
            });
        }
    }
    Ok(pages)
}

// ---------------------------------------------------------------------------
// 编排：候选 → 上下文 → LLM 综合 → 引用提取 → 沉淀
// ---------------------------------------------------------------------------

/// 查询完整编排（端口注入变体，公共 API）：真实路径
/// [`query`]（rig 端口）与测试路径（Fake 端口）共用全部编排逻辑，
/// 行为一致可离线验证（与 [`crate::ingest_file_with_ports`] 同模式）。
///
/// `on_event` 要求 `Send`：LLM 流式 future 跨线程轮询（见
/// [`LlmPort::complete_stream`]）。
pub async fn query_with_port<F>(
    vault_root: impl AsRef<Path>,
    question: impl Into<String>,
    port: &dyn LlmPort,
    config: &AppConfig,
    mut on_event: F,
) -> Result<QueryOutcome>
where
    F: FnMut(QueryEvent) + Send,
{
    let vault_root = vault_root.as_ref();
    let question = question.into();
    let question = question.trim().to_string();
    if question.is_empty() {
        return Err(Error::InvalidConfig("查询问题不能为空".to_string()));
    }
    if !vault_root.join("wiki").is_dir() {
        return Err(Error::NotAVault {
            path: vault_root.to_path_buf(),
            reason: "缺 wiki/ 目录".to_string(),
        });
    }

    // 第一级：词法候选（确定性，零 LLM）。
    let terms = tokenize_question(&question);
    on_event(QueryEvent::Searching { terms });
    let candidates = search_pages(vault_root, &question)?;

    // 无候选：不调 LLM、不沉淀（没有上下文的综合只会幻觉）。
    if candidates.is_empty() {
        on_event(QueryEvent::Done);
        return Ok(QueryOutcome {
            question,
            answer: NO_CANDIDATE_ANSWER.to_string(),
            candidates: Vec::new(),
            citations: Vec::new(),
            query_page_path: None,
            commit: None,
        });
    }

    // 第二级：候选正文 → LLM 综合（流式增量经节流上报）。
    let context = build_context(vault_root, &candidates)?;
    on_event(QueryEvent::Answering { chars: 0 });
    let answer = synthesize(vault_root, port, &question, &context, &mut |chars| {
        on_event(QueryEvent::Answering { chars })
    })
    .await?;
    let citations = extract_citations(&answer, &candidates);

    // 沉淀：Query page（配置可关闭；与 ingest apply 同一事务核心）。
    let (query_page_path, commit) = if config.query_pages {
        let page = build_query_page(vault_root, &question, &answer, &candidates)?;
        let commit = settle(vault_root, &page)?;
        (Some(page.path), Some(commit))
    } else {
        (None, None)
    };

    on_event(QueryEvent::Done);
    Ok(QueryOutcome {
        question,
        answer,
        candidates,
        citations,
        query_page_path,
        commit,
    })
}

/// 查询完整编排（真实端口变体，rig feature）：按全局配置的 strong 档
/// 构造端口（服务端输出上限 [`crate::QUERY_MAX_TOKENS`]），其余语义与
/// [`query_with_port`] 完全一致。桌面壳与 corpus-server 均委托本函数。
#[cfg(feature = "rig")]
pub async fn query<F>(
    vault_root: impl AsRef<Path>,
    question: impl Into<String>,
    config: &AppConfig,
    on_event: F,
) -> Result<QueryOutcome>
where
    F: FnMut(QueryEvent) + Send,
{
    let port = crate::rig_llm::build_llm_port_with_cap(
        &config.strong,
        Some(crate::llm::QUERY_MAX_TOKENS),
    )?;
    query_with_port(vault_root, question, &port, config, on_event).await
}

/// 拼装候选页上下文：每页一段，wikilink 形式标题 + 截断后的正文。
fn build_context(vault_root: &Path, candidates: &[ScoredPage]) -> Result<String> {
    let wiki = vault_root.join("wiki");
    let mut context = String::new();
    for candidate in candidates {
        let content = fs::read_to_string(wiki.join(&candidate.path)).unwrap_or_default();
        let body = match parse_page(&content) {
            Ok((_, body)) => body,
            // 检索时还能解析、此刻读不出的竞争情况：正文留空不阻断
            Err(_) => String::new(),
        };
        let link = candidate
            .path
            .strip_suffix(".md")
            .unwrap_or(&candidate.path);
        context.push_str(&format!(
            "\n## {}\n\n{}\n",
            wikilink(link, &candidate.title),
            truncate_chars(body.trim(), QUERY_CONTEXT_PAGE_MAX_CHARS)
        ));
    }
    Ok(context)
}

/// LLM 综合一次：流式增量经节流上报 chars（口径同 ingest，常量复用），
/// 聚合结果即回答（自由文本，无解析/重试层）；调用记入 llm-calls.log
///（用途 query，失败自动落 debug 日志——诊断口径与 ingest 一致）。
async fn synthesize(
    vault_root: &Path,
    port: &dyn LlmPort,
    question: &str,
    context: &str,
    on_chars: &mut (dyn FnMut(usize) + Send),
) -> Result<String> {
    let user = format!("# 问题\n\n{question}\n\n# 候选页面\n{context}");
    let mut recorder = crate::ingest::LlmCallRecorder::new(vault_root);
    let start = std::time::Instant::now();
    let mut chars = 0usize;
    let mut reported = 0usize;
    let mut last_report = std::time::Instant::now();
    let result = port
        .complete_stream(Some(ANSWER_SYSTEM_PROMPT), &user, &mut |delta| {
            chars += delta.chars().count();
            if last_report.elapsed() >= REPORT_MIN_INTERVAL
                || chars - reported >= REPORT_MIN_NEW_CHARS
            {
                on_chars(chars);
                reported = chars;
                last_report = std::time::Instant::now();
            }
        })
        .await;
    match result {
        Ok(answer) => {
            // 补一次终值（尾部增量可能未达节流阈值）
            if chars != reported {
                on_chars(chars);
            }
            recorder.record(LlmCallStats {
                ts: chrono::Local::now().to_rfc3339(),
                tier: LlmTier::Strong,
                purpose: LlmCallPurpose::Query,
                attempt: 1,
                system_chars: ANSWER_SYSTEM_PROMPT.chars().count(),
                user_chars: user.chars().count(),
                output_chars: answer.chars().count(),
                duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                outcome: LlmCallOutcome::Ok,
                error: None,
                output_head: None,
                output_tail: None,
                has_think_tag: None,
                salvaged: None,
            });
            Ok(answer)
        }
        Err(err) => {
            recorder.record(LlmCallStats {
                ts: chrono::Local::now().to_rfc3339(),
                tier: LlmTier::Strong,
                purpose: LlmCallPurpose::Query,
                attempt: 1,
                system_chars: ANSWER_SYSTEM_PROMPT.chars().count(),
                user_chars: user.chars().count(),
                output_chars: chars,
                duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
                outcome: LlmCallOutcome::Error,
                error: Some(truncate_chars(&err.to_string(), 200)),
                output_head: None,
                output_tail: None,
                has_think_tag: None,
                salvaged: None,
            });
            recorder.record_debug(
                LlmTier::Strong,
                LlmCallPurpose::Query,
                LlmCallOutcome::Error,
                Some(ANSWER_SYSTEM_PROMPT),
                &user,
                "",
                Some(&err.to_string()),
            );
            Err(err)
        }
    }
}

/// 从回答中提取引用：扫描 `[[target|label]]`，target 归一化后与候选页
/// 的路径（去 `.md`）或标题匹配；按回答中的出现顺序去重，未命中候选
/// 的目标忽略（防伪造引用混进引用列表）。
fn extract_citations(answer: &str, candidates: &[ScoredPage]) -> Vec<QueryCitation> {
    let mut out: Vec<QueryCitation> = Vec::new();
    let mut rest = answer;
    while let Some(pos) = rest.find("[[") {
        let after = &rest[pos + 2..];
        let Some(end) = after.find("]]") else {
            break; // 未闭合：其后不会再有完整链接
        };
        let inner = &after[..end];
        let target = inner.split_once('|').map_or(inner, |(t, _)| t).trim();
        if !target.is_empty() {
            let norm = normalize_name(target);
            let matched = candidates.iter().find(|c| {
                normalize_name(c.path.strip_suffix(".md").unwrap_or(&c.path)) == norm
                    || normalize_name(&c.title) == norm
            });
            if let Some(c) = matched {
                let citation = QueryCitation {
                    path: c.path.clone(),
                    title: c.title.clone(),
                };
                if !out.contains(&citation) {
                    out.push(citation);
                }
            }
        }
        rest = &after[end + 2..];
    }
    out
}

/// 组装 Query page：title = 问题截断；aliases = [问题]（title 自身在
/// 集合内由 [`merge_aliases`] 兜底）；related = 候选页 wikilink；
/// body = 完整回答；tags = [query]。同题（title 或别名归一化相等的
/// 既有 Query page）→ 更新原页（保留 path/created，别名并集）——重复
/// 追问不产生重复页面，与 ingest 精确名去重同哲学。
fn build_query_page(
    vault_root: &Path,
    question: &str,
    answer: &str,
    candidates: &[ScoredPage],
) -> Result<PageDraft> {
    let title = truncate_chars(question, QUERY_TITLE_MAX_CHARS);
    let now = chrono::Local::now().to_rfc3339();
    let existing = find_existing_query(vault_root, &title);
    let (path, created, mut aliases) = match &existing {
        Some(old) => (old.0.clone(), old.1.created.clone(), old.1.aliases.clone()),
        None => (
            allocate_query_path(vault_root, &title),
            now.clone(),
            Vec::new(),
        ),
    };
    merge_aliases(&mut aliases, &[question.to_string()], &title);
    let related = candidates
        .iter()
        .map(|c| wikilink(c.path.strip_suffix(".md").unwrap_or(&c.path), &c.title))
        .collect();
    Ok(PageDraft {
        action: if existing.is_some() {
            PageAction::Update
        } else {
            PageAction::Create
        },
        path,
        frontmatter: PageFrontmatter {
            page_type: PageKind::Query,
            title,
            aliases,
            tags: vec![PageKind::Query.as_str().to_string()],
            related,
            // Query page 无 raw 溯源指针：它的来源就是这次提问本身
            sources: Vec::new(),
            created,
            updated: now,
            raw_path: None,
            fingerprint: None,
        },
        body: answer.trim_end().to_string(),
    })
}

/// 同题既有 Query page（路径 + frontmatter）：title 或任一别名与
/// `title` 归一化相等即命中；解析失败的单页跳过。
fn find_existing_query(vault_root: &Path, title: &str) -> Option<(String, PageFrontmatter)> {
    let dir = vault_root.join("wiki").join(PageKind::Query.dir());
    let norm = normalize_name(title);
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok((frontmatter, _)) = parse_page(&content) else {
            continue;
        };
        let hit = normalize_name(&frontmatter.title) == norm
            || frontmatter
                .aliases
                .iter()
                .any(|a| normalize_name(a) == norm);
        if hit {
            let rel = path
                .strip_prefix(vault_root.join("wiki"))
                .expect("queries 下的页面必然能 strip wiki/ 前缀")
                .to_string_lossy()
                .replace('\\', "/");
            return Some((rel, frontmatter));
        }
    }
    None
}

/// 新 Query page 的路径分配：slug 化 + 目录内撞车追加序号（`-2`、
/// `-3`…），与 ingest 的路径分配语义一致。
fn allocate_query_path(vault_root: &Path, title: &str) -> String {
    let dir = PageKind::Query.dir();
    let base = slugify(title);
    let queries = vault_root.join("wiki").join(dir);
    let mut candidate = base.clone();
    let mut counter = 1;
    while queries.join(format!("{candidate}.md")).exists() {
        counter += 1;
        candidate = format!("{base}-{counter}");
    }
    format!("{dir}/{candidate}.md")
}

/// 沉淀落盘：与 ingest apply 同一事务核心（原子写 + index 全量重建 +
/// log 追加 + 恰好一个提交，失败恢复快照零半写）。
fn settle(vault_root: &Path, page: &PageDraft) -> Result<String> {
    let message = format!("{QUERY_COMMIT_PREFIX} | {}", page.frontmatter.title);
    let today = chrono::Local::now().format("%Y-%m-%d");
    let log_entry = format!("## [{today}] query | {}\n", page.frontmatter.title);
    write_pages_and_commit(
        vault_root,
        std::slice::from_ref(page),
        &[],
        &message,
        &log_entry,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_cjk_ascii_and_mixed() {
        // ASCII 按词（小写化）
        assert_eq!(
            tokenize_question("What is Transformer?"),
            vec![
                "is".to_string(),
                "transformer".to_string(),
                "what".to_string()
            ]
        );
        // CJK 单字 + 相邻二元组（去重 + 码点排序）
        assert_eq!(
            tokenize_question("自注意"),
            vec![
                "意".to_string(),
                "注".to_string(),
                "注意".to_string(),
                "自".to_string(),
                "自注".to_string(),
            ]
        );
        // 混合：ASCII 词 + CJK 段展开；标点为分隔
        assert_eq!(
            tokenize_question("TCP 协议"),
            vec![
                "tcp".to_string(),
                "协".to_string(),
                "协议".to_string(),
                "议".to_string(),
            ]
        );
        // 纯标点 / 空白：无词项
        assert!(tokenize_question("？！ -").is_empty());
    }

    #[test]
    fn score_page_weights_title_alias_body_and_caps() {
        let terms = vec!["transformer".to_string(), "attention".to_string()];
        // title 命中两个词（20）> 别名命中（12）> 仅正文
        let title_only = score_page(&terms, "Attention Transformer", &["X".to_string()], "");
        assert_eq!(title_only, 20);
        let alias_only = score_page(&terms, "架构", &["Attention Transformer".to_string()], "");
        assert_eq!(alias_only, 12);
        // 正文命中计数封顶 5（长文刷不动分）
        let body = "attention ".repeat(20);
        let body_only = score_page(&terms, "无关", &[], &body);
        assert_eq!(body_only, 5);
        // 完全无命中
        assert_eq!(
            score_page(&terms, "别的", &["其他".to_string()], "毫无相关内容"),
            0
        );
        // 不区分大小写
        assert_eq!(score_page(&["tcp".to_string()], "TCP 协议", &[], ""), 10);
    }

    #[test]
    fn extract_citations_match_dedupe_and_ignore_unknown() {
        let candidates = vec![
            ScoredPage {
                path: "entities/transformer.md".into(),
                title: "Transformer".into(),
                score: 20,
            },
            ScoredPage {
                path: "concepts/自注意力.md".into(),
                title: "自注意力".into(),
                score: 12,
            },
        ];
        let answer = "见 [[entities/transformer|Transformer]] 与 [[自注意力]]；\
                      伪造的 [[concepts/幽灵]] 不算，重复的 [[entities/transformer]] 去重。";
        assert_eq!(
            extract_citations(answer, &candidates),
            vec![
                QueryCitation {
                    path: "entities/transformer.md".into(),
                    title: "Transformer".into(),
                },
                QueryCitation {
                    path: "concepts/自注意力.md".into(),
                    title: "自注意力".into(),
                },
            ]
        );
        // 无链接 → 无引用；未闭合链接安全终止
        assert!(extract_citations("没有任何链接", &candidates).is_empty());
        assert!(extract_citations("悬空 [[断掉", &candidates).is_empty());
    }
}
