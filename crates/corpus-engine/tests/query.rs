//! 查询最小闭环集成测试（票据 T9 验收）：词法候选确定性（中/英/别名/
//! 无命中/TopN 截断）+ 查询全链（fake 端口：候选正文注入断言、流式
//! chars 事件、Query page 落盘七字段断言、配置关闭不落盘、无候选不调
//! LLM）。
//!
//! 端口注入走 [`query_with_port`]（编排函数的显式端口参数，与
//! `ingest_file_with_ports` 同模式）：Scripted / Streaming Fake 共用
//! 真实编排逻辑，行为一致可离线验证。

mod common;

use std::fs;
use std::sync::{Arc, Mutex};

use common::{fresh_vault_path, git, ScriptedLlmPort, StreamingLlmPort};
use corpus_engine::config::{AppConfig, ApplyMode, ModelTier, ProviderKind};
use corpus_engine::{
    create_vault, ingest_file_with_ports, query_with_port, search_pages, PageKind, QueryEvent,
    QueryOutcome, TemplateId, NO_CANDIDATE_ANSWER, SEARCH_TOP_N,
};

// ---------------------------------------------------------------------------
// 测试脚手架
// ---------------------------------------------------------------------------

/// 造一个绑定 Generic 模板的全新 Vault。
fn setup_vault(tag: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let (dir, root) = fresh_vault_path(tag);
    create_vault(&root, &TemplateId::from("generic")).expect("create_vault 失败");
    (dir, root)
}

/// Fake 档位（api_key 留空——Fake 端口不解析 key）。
fn fake_tier() -> ModelTier {
    ModelTier {
        provider: ProviderKind::OpenAI,
        model: "fake-model".to_string(),
        base_url: None,
        api_key: None,
    }
}

fn config_with(query_pages: bool) -> AppConfig {
    AppConfig {
        fast: fake_tier(),
        strong: fake_tier(),
        apply_mode: ApplyMode::Auto,
        query_pages,
    }
}

/// 第一步（fast 档）的标准分析输出（与 tests/ingest_file.rs 同脚本）。
fn analysis_json() -> String {
    r#"{
  "source_title": "Attention Is All You Need",
  "source_summary": "提出 Transformer 架构的论文，用自注意力取代循环。",
  "entities": [
    {"title": "Transformer", "aliases": ["Transformer 架构"], "tags": ["deep-learning"],
     "summary": "完全基于注意力的序列模型架构。", "related": ["自注意力"]}
  ],
  "concepts": [
    {"title": "自注意力", "aliases": ["Self-Attention"], "tags": [],
     "summary": "让序列中每个位置直接关注全部位置的机制。", "related": ["Transformer"]}
  ]
}"#
    .to_string()
}

/// 第二步（strong 档）的标准成文输出（与 tests/ingest_file.rs 同脚本）。
fn bodies_json() -> String {
    r##"{
  "pages": [
    {"path": "sources/attention-is-all-you-need",
     "body": "摘要：提出 [[entities/transformer|Transformer]] 架构的论文。"},
    {"path": "entities/transformer",
     "body": "Transformer 是完全基于 [[concepts/自注意力]] 的架构。"},
    {"path": "concepts/自注意力",
     "body": "自注意力是 [[entities/transformer|Transformer]] 的核心机制。"}
  ]
}"##
    .to_string()
}

/// 摄取标准样例（三页：Source / Transformer / 自注意力），auto 落盘。
async fn ingest_sample(root: &std::path::Path) {
    let (ext_dir, ext_root) = fresh_vault_path("external-source");
    fs::create_dir_all(&ext_root).expect("建外部目录失败");
    let src = ext_root.join("paper.md");
    fs::write(&src, "论文正文……").expect("写外部源文件失败");
    let fast = ScriptedLlmPort::new(vec![analysis_json()]);
    let strong = ScriptedLlmPort::new(vec![bodies_json()]);
    let report = ingest_file_with_ports(root, &src, &config_with(true), &fast, &strong, |_| {})
        .await
        .expect("样例摄取失败");
    assert!(report.applied, "样例摄取应已落盘");
    let _ = ext_dir;
}

/// 手写一个合法 entity 页面（搜索/上下文测试用，绕开摄取脚本）。
fn write_entity_page(root: &std::path::Path, slug: &str, title: &str, body: &str) {
    let entities = root.join("wiki/entities");
    fs::create_dir_all(&entities).expect("建 entities 目录失败");
    let content = format!(
        "---\ntype: entity\ntitle: {title}\naliases: [{title}]\ntags: []\nrelated: []\nsources: []\ncreated: t\nupdated: t\n---\n\n{body}\n"
    );
    fs::write(entities.join(format!("{slug}.md")), content).expect("写页面失败");
}

/// 收集查询事件的容器。
fn event_sink() -> (Arc<Mutex<Vec<QueryEvent>>>, impl FnMut(QueryEvent) + Send) {
    let events: Arc<Mutex<Vec<QueryEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let on_event = move |ev: QueryEvent| {
        sink.lock().unwrap().push(ev);
    };
    (events, on_event)
}

/// 标准综合回答（含两条 wikilink 引用）。
fn answer_text() -> String {
    "自注意力让序列中每个位置直接关注全部位置，见 [[concepts/自注意力|自注意力]]。\n\n\
     论文背景见 [[sources/attention-is-all-you-need|Attention Is All You Need]]。"
        .to_string()
}

// ---------------------------------------------------------------------------
// 词法层（确定性，零 LLM）
// ---------------------------------------------------------------------------

/// 验收：词法候选页选取不依赖 LLM——title / 别名 / 英文命中与无命中。
#[tokio::test]
async fn search_pages_ranks_by_title_alias_and_language() {
    let (_dir, root) = setup_vault("vault");
    ingest_sample(&root).await;

    // 中文 title 命中：Transformer 页第一（title 10 + 别名 6 + 正文计数）
    let hits = search_pages(&root, "Transformer").expect("搜索失败");
    assert!(!hits.is_empty());
    assert_eq!(hits[0].path, "entities/transformer.md", "{hits:?}");

    // 别名命中：纯英文问题的候选里，别名 Self-Attention 让自注意力页
    // 拿到别名分；但换纯中文词可单看 title/别名区分——「自注意力」在
    // 该页 title 上全词命中（60 分），远超任何别页
    let hits = search_pages(&root, "自注意力").expect("搜索失败");
    assert_eq!(hits[0].path, "concepts/自注意力.md", "{hits:?}");

    // 别名（跨语言）命中：该页正文不含 attention，别名 Self-Attention 含
    let hits = search_pages(&root, "Self-Attention").expect("搜索失败");
    assert!(
        hits.iter().any(|h| h.path == "concepts/自注意力.md"),
        "别名命中应入选：{hits:?}"
    );

    // 英文命中：source 页 title 含 attention
    let hits = search_pages(&root, "attention").expect("搜索失败");
    assert_eq!(
        hits[0].path, "sources/attention-is-all-you-need.md",
        "{hits:?}"
    );

    // 无命中：任何页面都不含这些字
    let hits = search_pages(&root, "量子纠缠").expect("搜索失败");
    assert!(hits.is_empty(), "{hits:?}");

    // 纯标点 / 空白问题：无词项，空候选
    assert!(search_pages(&root, "？！").expect("搜索失败").is_empty());
}

/// 验收：TopN 截断——命中页多于 [`SEARCH_TOP_N`] 时只取前 6，且同分
/// 按路径升序稳定排序（重复搜索结果完全一致）。
#[tokio::test]
async fn search_pages_truncates_to_top_n_deterministically() {
    let (_dir, root) = setup_vault("vault");
    for i in 0..8 {
        write_entity_page(
            &root,
            &format!("p{i}"),
            &format!("页面{i}"),
            "共同关键词正文",
        );
    }
    let hits = search_pages(&root, "关键词").expect("搜索失败");
    assert_eq!(hits.len(), SEARCH_TOP_N, "命中 8 页应截断到 TopN：{hits:?}");
    let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(
        paths,
        [
            "entities/p0.md",
            "entities/p1.md",
            "entities/p2.md",
            "entities/p3.md",
            "entities/p4.md",
            "entities/p5.md",
        ],
        "同分页按路径升序：{hits:?}"
    );
    // 相同输入重复搜索：结果完全一致（确定性）
    let again = search_pages(&root, "关键词").expect("二次搜索失败");
    assert_eq!(hits, again);
}

/// 验收：非 Vault 与坏页容错——非 Vault 报错；坏 frontmatter 页跳过
/// 不阻断搜索。
#[tokio::test]
async fn search_pages_rejects_non_vault_and_tolerates_broken_pages() {
    let (_dir, root) = setup_vault("vault");
    let err = search_pages(root.join("not-a-vault"), "问题").expect_err("非 Vault 应报错");
    assert!(
        matches!(err, corpus_engine::Error::NotAVault { .. }),
        "{err:?}"
    );

    write_entity_page(&root, "good", "好页", "关键词");
    fs::create_dir_all(root.join("wiki/entities")).expect("建目录失败");
    fs::write(
        root.join("wiki/entities/broken.md"),
        "---\ntitle: 缺 aliases 的坏页\n---\n\n关键词\n",
    )
    .expect("写坏页失败");
    let hits = search_pages(&root, "关键词").expect("坏页不应阻断搜索");
    assert_eq!(hits.len(), 1, "只有好页命中：{hits:?}");
    assert_eq!(hits[0].path, "entities/good.md");
}

// ---------------------------------------------------------------------------
// 查询全链（fake 端口）
// ---------------------------------------------------------------------------

/// 验收（全链）：级联调用了词法层且只把候选页正文注入上下文；流式
/// chars 事件按 Searching → Answering(递增) → Done 到达；回答中的引用
/// 解析为真实页面；Query page 落盘（七字段）+ log + index 富行 + 恰好
/// 一个提交。
#[tokio::test]
async fn query_full_chain_streams_cites_and_settles() {
    let (_dir, root) = setup_vault("vault");
    ingest_sample(&root).await;
    // 与问题零词项重叠的干扰页（验证只有候选页正文被注入）
    write_entity_page(
        &root,
        "bert",
        "BERT",
        "BERT is an encoder-only model by Google.",
    );
    let commits_before = git(&root, &["rev-list", "--count", "HEAD"]);

    let question = "Self-Attention 的机制是什么？";
    let port = StreamingLlmPort::over(vec![answer_text()], 12, 1);
    let (events, on_event) = event_sink();
    let outcome: QueryOutcome =
        query_with_port(&root, question, &port, &config_with(true), on_event)
            .await
            .expect("查询失败");

    // 级联：恰好一次 LLM 调用，user 只含候选页正文
    assert_eq!(port.call_count(), 1, "应恰好一次综合调用");
    let (system, user) = &port.recorded_calls()[0];
    assert!(
        system
            .as_deref()
            .is_some_and(|s| s.contains("ONLY the candidate pages")),
        "system 指示应约束只基于候选页回答：{system:?}"
    );
    assert!(
        user.contains("自注意力是 [[entities/transformer|Transformer]] 的核心机制。"),
        "候选页（自注意力）正文应注入：{user}"
    );
    assert!(
        user.contains("摘要：提出 [[entities/transformer|Transformer]] 架构的论文。"),
        "候选页（source）正文应注入：{user}"
    );
    assert!(
        user.contains("完全基于"),
        "候选页（Transformer，正文命中）应注入：{user}"
    );
    assert!(
        !user.contains("encoder-only"),
        "非候选页（BERT，零分）正文不得注入：{user}"
    );
    assert!(user.contains(question), "问题应原样进入 user 消息");

    // 流式事件：Searching（带词项）→ Answering（0 → 递增）→ Done
    let events = events.lock().unwrap().clone();
    assert!(
        matches!(&events[0], QueryEvent::Searching { terms } if !terms.is_empty()),
        "{events:?}"
    );
    assert!(
        matches!(events[events.len() - 1], QueryEvent::Done),
        "{events:?}"
    );
    let chars_seq: Vec<usize> = events
        .iter()
        .filter_map(|ev| match ev {
            QueryEvent::Answering { chars } => Some(*chars),
            _ => None,
        })
        .collect();
    assert_eq!(
        chars_seq.first(),
        Some(&0),
        "Answering 应以 chars=0 开始：{events:?}"
    );
    assert!(
        chars_seq.windows(2).all(|w| w[0] <= w[1]),
        "chars 单调递增：{chars_seq:?}"
    );
    assert!(
        *chars_seq.last().expect("应有 Answering 事件") > 0,
        "流式应有非零增量：{chars_seq:?}"
    );

    // 结果：回答 + 引用（按回答出现顺序，只含真实 wikilink 到的候选页）
    assert_eq!(outcome.answer, answer_text());
    assert_eq!(
        outcome
            .citations
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        [
            "concepts/自注意力.md",
            "sources/attention-is-all-you-need.md"
        ],
        "引用解析：{:?}",
        outcome.citations
    );
    let page_rel = outcome.query_page_path.clone().expect("应沉淀 Query page");
    assert!(page_rel.starts_with("queries/"), "{page_rel}");
    assert!(outcome.commit.is_some());

    // Query page 落盘断言：type/title/aliases/tags/related/sources/created/updated/body
    let raw = fs::read_to_string(root.join("wiki").join(&page_rel)).expect("Query page 应存在");
    let (fm, body) = corpus_engine::parse_page(&raw).expect("Query page 应合法");
    assert_eq!(fm.page_type, PageKind::Query);
    assert_eq!(fm.title, question, "title = 问题截断（本题未超长）");
    assert!(
        fm.aliases.iter().any(|a| a == question),
        "aliases 应含问题原文：{:?}",
        fm.aliases
    );
    assert_eq!(fm.tags, vec!["query".to_string()]);
    assert!(
        fm.related.iter().any(|r| r.contains("concepts/自注意力"))
            && fm
                .related
                .iter()
                .any(|r| r.contains("sources/attention-is-all-you-need")),
        "related 应含候选页 wikilink：{:?}",
        fm.related
    );
    assert_eq!(fm.sources, Vec::<String>::new(), "Query page 无 raw 溯源");
    assert!(!fm.created.is_empty() && !fm.updated.is_empty());
    assert_eq!(body.trim_end(), answer_text().trim_end(), "body = 完整回答");

    // log + index 富行 + 恰好一个 query 提交
    let log = fs::read_to_string(root.join("wiki/log.md")).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert!(
        log.contains(&format!("## [{today}] query | {question}")),
        "log.md 应含 query 记录：{log}"
    );
    let index = fs::read_to_string(root.join("wiki/index.md")).unwrap();
    let digest_head = "自注意力让序列中每个位置直接关注全部位置，见 自注意力。";
    let link = page_rel.strip_suffix(".md").unwrap_or(&page_rel);
    assert!(
        index.contains(&format!("- [[{link}|{question}]] — {digest_head}")),
        "index 应含 Query page 富行（wikilink + 摘要，无源数段）：{index}"
    );
    let subject = git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(
        subject,
        format!("corpus: query | {question}"),
        "提交信息应为 query 前缀"
    );
    assert_eq!(
        git(&root, &["rev-list", "--count", "HEAD"]),
        (commits_before.parse::<u32>().expect("提交计数") + 1).to_string(),
        "恰好新增一个提交"
    );
    let files = git(&root, &["show", "--name-only", "--format=", "HEAD"]);
    assert!(
        files.lines().any(|l| l == format!("wiki/{page_rel}"))
            && files.contains("wiki/index.md")
            && files.contains("wiki/log.md"),
        "提交应含 Query page + index + log：\n{files}"
    );
}

/// 验收：同一问题重复提问 → 精确名去重更新原页（不产生重复 Query
/// page），再添一个提交。
#[tokio::test]
async fn repeated_question_updates_existing_query_page() {
    let (_dir, root) = setup_vault("vault");
    ingest_sample(&root).await;

    let question = "Transformer 是什么？";
    let first = query_with_port(
        &root,
        question,
        &ScriptedLlmPort::new(vec!["初版回答。".to_string()]),
        &config_with(true),
        |_| {},
    )
    .await
    .expect("首次查询失败");
    let path = first.query_page_path.clone().expect("应沉淀");

    let second = query_with_port(
        &root,
        question,
        &ScriptedLlmPort::new(vec!["更新版回答。".to_string()]),
        &config_with(true),
        |_| {},
    )
    .await
    .expect("重复查询失败");
    assert_eq!(second.query_page_path, Some(path.clone()), "同题应更新原页");

    let queries = root.join("wiki/queries");
    let count = fs::read_dir(&queries)
        .expect("queries 目录应存在")
        .flatten()
        .filter(|e| e.path().extension().and_then(|e| e.to_str()) == Some("md"))
        .count();
    assert_eq!(count, 1, "重复提问不得产生重复页面");
    let raw = fs::read_to_string(root.join("wiki").join(&path)).unwrap();
    assert!(raw.contains("更新版回答。"), "原页应被更新：{raw}");
}

/// 验收：`query_pages = false` → 回答照常、LLM 照调，但零落盘。
#[tokio::test]
async fn query_pages_disabled_skips_settlement() {
    let (_dir, root) = setup_vault("vault");
    ingest_sample(&root).await;
    let commits_before = git(&root, &["rev-list", "--count", "HEAD"]);

    let port = ScriptedLlmPort::new(vec!["见 [[entities/transformer|Transformer]]。".to_string()]);
    let outcome = query_with_port(&root, "Transformer", &port, &config_with(false), |_| {})
        .await
        .expect("查询失败");

    assert_eq!(port.call_count(), 1, "LLM 照常调用");
    assert_eq!(outcome.answer, "见 [[entities/transformer|Transformer]]。");
    assert_eq!(
        outcome
            .citations
            .iter()
            .map(|c| c.path.as_str())
            .collect::<Vec<_>>(),
        ["entities/transformer.md"],
        "引用照常提取"
    );
    assert_eq!(outcome.query_page_path, None, "不沉淀");
    assert_eq!(outcome.commit, None, "无提交");
    assert!(!root.join("wiki/queries").exists(), "queries/ 不应被创建");
    assert_eq!(
        git(&root, &["rev-list", "--count", "HEAD"]),
        commits_before,
        "零提交"
    );
}

/// 验收：无候选页 → 不调 LLM、不沉淀，返回固定回答（事件仍完整走
/// Searching → Done）。
#[tokio::test]
async fn query_without_candidates_skips_llm_and_settlement() {
    let (_dir, root) = setup_vault("vault");
    let port = ScriptedLlmPort::new(vec![]); // 脚本为空：任何调用都会炸
    let (events, on_event) = event_sink();

    let outcome = query_with_port(&root, "量子纠缠是什么", &port, &config_with(true), on_event)
        .await
        .expect("无候选应返回固定回答而非报错");

    assert_eq!(port.call_count(), 0, "无候选不得调 LLM");
    assert_eq!(outcome.answer, NO_CANDIDATE_ANSWER);
    assert!(outcome.candidates.is_empty() && outcome.citations.is_empty());
    assert_eq!(outcome.query_page_path, None);
    assert_eq!(outcome.commit, None);
    assert!(!root.join("wiki/queries").exists(), "无候选不落盘");
    let events = events.lock().unwrap().clone();
    assert_eq!(
        events.len(),
        2,
        "无候选路径只有 Searching + Done：{events:?}"
    );
    assert!(
        matches!(&events[0], QueryEvent::Searching { terms } if !terms.is_empty()),
        "{events:?}"
    );
    assert_eq!(events[1], QueryEvent::Done);
}

/// 验收：空白问题与非 Vault 报错。
#[tokio::test]
async fn query_rejects_blank_question_and_non_vault() {
    let (_dir, root) = setup_vault("vault");
    let port = ScriptedLlmPort::new(vec![]);

    let err = query_with_port(&root, "   ", &port, &config_with(true), |_| {})
        .await
        .expect_err("空白问题应报错");
    assert!(
        matches!(err, corpus_engine::Error::InvalidConfig(_)),
        "{err:?}"
    );

    let err = query_with_port(
        root.join("nowhere"),
        "问题",
        &port,
        &config_with(true),
        |_| {},
    )
    .await
    .expect_err("非 Vault 应报错");
    assert!(
        matches!(err, corpus_engine::Error::NotAVault { .. }),
        "{err:?}"
    );
    assert_eq!(port.call_count(), 0);
}
