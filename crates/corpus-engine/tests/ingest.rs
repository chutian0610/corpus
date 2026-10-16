//! Ingest 集成测试（票据 T3 验收）：只经引擎公共 API 驱动，LLM 接缝用
//! Scripted Fake（离线、确定性），断言落在真实文件系统（页面内容、
//! frontmatter schema、git 历史、raw/ 不变量）与 wikilink 回读。

mod common;

use std::fs;
use std::path::Path;

use common::{fresh_vault_path, git, tree_snapshot, ScriptedLlmPort};
use corpus_engine::{
    create_vault, ingest_file, parse_page, Error, IngestReport, IngestStage, PageAction, PageKind,
    TemplateId,
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

/// 写入 raw 文件，返回相对路径（如 `raw/a.md`）。
fn write_raw(root: &Path, name: &str, content: &str) -> String {
    let rel = format!("raw/{name}");
    fs::write(root.join(&rel), content).expect("写 raw 失败");
    rel
}

/// 第一步（fast 档）的标准分析输出：Transformer 论文一个实体 + 一个概念。
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

/// 第二步（strong 档）的标准成文输出：与 analysis_json 的 slug 对应。
/// 注意：value 不得以 `##` 开头（`"#`/`"##` 会撞 raw string 定界符）。
fn bodies_json() -> String {
    r##"{
  "pages": [
    {"path": "sources/attention-is-all-you-need",
     "body": "摘要：提出 [[entities/transformer|Transformer]] 架构的论文，核心是 [[concepts/自注意力]]。"},
    {"path": "entities/transformer",
     "body": "Transformer 是完全基于 [[concepts/自注意力]] 的架构。见 [[sources/attention-is-all-you-need|Attention Is All You Need]]。"},
    {"path": "concepts/自注意力",
     "body": "自注意力是 [[entities/transformer|Transformer]] 的核心机制。"}
  ]
}"##
        .to_string()
}

/// fast + strong 各一条脚本的标准两步 fake。
fn standard_llms() -> (ScriptedLlmPort, ScriptedLlmPort) {
    (
        ScriptedLlmPort::new(vec![analysis_json()]),
        ScriptedLlmPort::new(vec![bodies_json()]),
    )
}

/// 忽略进度的空回调。
fn no_progress() -> impl FnMut(IngestStage) {
    |_: IngestStage| {}
}

/// 运行一次完整摄取（标准脚本）。
async fn ingest_ok(root: &Path, raw_rel: &str) -> IngestReport {
    let (fast, strong) = standard_llms();
    ingest_file(root, raw_rel, &fast, &strong, no_progress())
        .await
        .expect("标准脚本摄取应成功")
}

// ---------------------------------------------------------------------------
// 页面 schema 与产出结构
// ---------------------------------------------------------------------------

/// 验收：摄取后 1 个 Source page + Entity/Concept 页，七字段齐全、
/// aliases 非空、sources 指回 Source page、raw 指针正确。
#[tokio::test]
async fn ingest_creates_source_and_typed_pages_with_full_schema() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "paper.md", "论文正文……");
    let report = ingest_ok(&root, &raw).await;

    assert!(!report.skipped);
    // 产出清单：1 Source + 1 Entity + 1 Concept
    assert_eq!(report.pages.len(), 3);
    assert_eq!(report.pages[0].page_type, PageKind::Source);
    assert_eq!(report.pages[1].page_type, PageKind::Entity);
    assert_eq!(report.pages[2].page_type, PageKind::Concept);
    assert_eq!(report.pages[0].action, PageAction::Create);

    // Source page：raw 指针 + 指纹 + 摘要 + 产出索引
    let source_path = root.join("wiki").join(&report.pages[0].path);
    let (source_fm, source_body) =
        parse_page(&fs::read_to_string(&source_path).unwrap()).expect("Source page 解析失败");
    assert_eq!(source_fm.page_type, PageKind::Source);
    assert_eq!(source_fm.raw_path.as_deref(), Some(raw.as_str()));
    assert_eq!(
        source_fm.fingerprint.as_deref(),
        Some(report.fingerprint.as_str())
    );
    assert!(source_body.contains("摘要"), "Source 正文应含 LLM 摘要");
    assert!(
        source_body.contains("## 产出页面"),
        "Source 正文应含产出索引"
    );
    assert!(source_body.contains("[[entities/transformer|Transformer]]"));

    // 每个产出页面：七字段齐全、aliases 非空
    for page in &report.pages {
        let path = root.join("wiki").join(&page.path);
        let (fm, body) = parse_page(&fs::read_to_string(&path).unwrap()).expect("页面解析失败");
        assert_eq!(fm.page_type, page.page_type);
        assert_eq!(fm.title, page.title);
        assert!(!fm.aliases.is_empty(), "{}: aliases 不得为空", page.path);
        assert!(!fm.created.is_empty());
        assert!(!fm.updated.is_empty());
        assert!(
            !fm.related.is_empty(),
            "{}: related 应解析出批内关联",
            page.path
        );
        assert!(!body.trim().is_empty());
        // Entity/Concept 的 sources 指回 Source page（wikilink 形式）
        if page.page_type != PageKind::Source {
            assert_eq!(
                fm.sources,
                vec!["[[sources/attention-is-all-you-need]]".to_string()],
                "{}: sources 应精确指回 Source page",
                page.path
            );
        }
    }
    // 实体页 aliases 含分析给的别名与 title
    let entity = report
        .pages
        .iter()
        .find(|p| p.page_type == PageKind::Entity)
        .unwrap();
    let (fm, _) =
        parse_page(&fs::read_to_string(root.join("wiki").join(&entity.path)).unwrap()).unwrap();
    assert!(fm.aliases.contains(&"Transformer 架构".to_string()));
    assert!(fm.aliases.contains(&"Transformer".to_string()));
}

// ---------------------------------------------------------------------------
// 指纹跳过
// ---------------------------------------------------------------------------

/// 验收：重复摄取未变文件被指纹跳过，零 LLM 调用、零提交。
#[tokio::test]
async fn unchanged_fingerprint_skips_entire_pipeline() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "内容");
    let first = ingest_ok(&root, &raw).await;
    assert!(!first.skipped);
    let commits_after_first = git(&root, &["rev-list", "--count", "HEAD"]);

    let (fast, strong) = standard_llms();
    let second = ingest_file(&root, &raw, &fast, &strong, no_progress())
        .await
        .expect("重复摄取应成功返回（跳过路径）");
    assert!(second.skipped, "指纹未变必须跳过");
    assert_eq!(fast.call_count(), 0, "跳过时 fast 档零调用");
    assert_eq!(strong.call_count(), 0, "跳过时 strong 档零调用");
    assert_eq!(second.commit, None);
    assert_eq!(
        git(&root, &["rev-list", "--count", "HEAD"]),
        commits_after_first,
        "跳过不得产生新提交"
    );
    let status = git(&root, &["status", "--porcelain"]);
    assert!(status.is_empty(), "跳过后工作区应干净：{status}");
}

// ---------------------------------------------------------------------------
// 两步调用与档位
// ---------------------------------------------------------------------------

/// 验收：两步调用各恰好一次且档位正确（fast 分析 / strong 成文），
/// fast 的 system prompt 注入模板 schema_prompt；进度回调阶段有序。
#[tokio::test]
async fn two_llm_steps_each_once_with_correct_tier_and_schema_prompt() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");

    let (fast, strong) = standard_llms();
    let mut stages: Vec<String> = Vec::new();
    let report = ingest_file(&root, &raw, &fast, &strong, |stage| {
        stages.push(
            serde_json::to_value(&stage).unwrap()["stage"]
                .as_str()
                .unwrap()
                .to_string(),
        );
    })
    .await
    .expect("摄取失败");

    assert!(!report.skipped);
    // fast：恰好一次，system 含 schema_prompt，user 含 raw 全文
    assert_eq!(fast.call_count(), 1, "第一步（分析）应恰好调用一次");
    let (fast_system, fast_user) = &fast.recorded_calls()[0];
    let fast_system = fast_system
        .as_deref()
        .expect("分析步必须携带 system prompt");
    assert!(
        fast_system.contains("You are structuring knowledge for a generic personal wiki"),
        "fast 档 system 必须注入 Generic 模板 schema_prompt"
    );
    assert!(
        fast_system.contains("Output contract"),
        "JSON 输出约束应随 system 下发"
    );
    assert!(
        fast_user.contains("论文正文内容"),
        "raw 全文应作为 user 消息"
    );

    // strong：恰好一次，成文指令在 system，页面计划在 user
    assert_eq!(strong.call_count(), 1, "第二步（成文）应恰好调用一次");
    let (strong_system, strong_user) = &strong.recorded_calls()[0];
    assert!(strong_system
        .as_deref()
        .unwrap()
        .contains("wiki page writer"));
    assert!(strong_user.contains("# Page plan"));
    assert!(strong_user.contains("论文正文内容"));

    // 进度阶段：读取 → 分析 → 生成 → 落盘（依序且齐全）
    assert_eq!(
        stages,
        vec!["reading", "analyzing", "generating", "applying"]
    );
}

// ---------------------------------------------------------------------------
// 解析容错与零写入
// ---------------------------------------------------------------------------

/// 验收：坏 JSON 重试一次后失败，且 wiki/、git、raw/ 与摄取前完全一致。
#[tokio::test]
async fn bad_analysis_json_retries_once_then_fails_without_writes() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "内容");
    let state = failure_baseline(&root);

    let fast = ScriptedLlmPort::new(vec!["这不是 JSON".to_string(), "仍然不是 JSON".to_string()]);
    let strong = ScriptedLlmPort::new(vec![]);
    let err = ingest_file(&root, &raw, &fast, &strong, no_progress())
        .await
        .expect_err("两次坏输出必须报错");
    assert!(matches!(err, Error::BadLlmOutput(_)), "got: {err:?}");
    assert_eq!(fast.call_count(), 2, "失败应恰好重试一次（共两次）");
    assert_eq!(strong.call_count(), 0, "分析失败不得进入第二步");
    assert_failure_clean(&root, &state);
}

/// 验收：第二步失败（票据指定场景）不产生半写状态。
#[tokio::test]
async fn compose_failure_leaves_no_partial_writes() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "内容");
    let state = failure_baseline(&root);

    let fast = ScriptedLlmPort::new(vec![analysis_json()]);
    let strong = ScriptedLlmPort::new(vec!["格式错误".to_string(), "还是格式错误".to_string()]);
    let err = ingest_file(&root, &raw, &fast, &strong, no_progress())
        .await
        .expect_err("第二步坏输出必须报错");
    assert!(matches!(err, Error::BadLlmOutput(_)), "got: {err:?}");
    assert_eq!(fast.call_count(), 1);
    assert_eq!(strong.call_count(), 2, "成文步应重试一次后失败");
    assert_failure_clean(&root, &state);
}

/// 首次输出坏、重试后好：管道恢复并成功落盘（容错重试的价值路径）。
#[tokio::test]
async fn retry_recovers_from_one_bad_output() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "内容");
    let fast = ScriptedLlmPort::new(vec![
        // 截断的 JSON：能提取到片段但无法解析 → 触发一次重试
        "{\"source_title\": \"T\"".to_string(),
        analysis_json(),
    ]);
    let strong = ScriptedLlmPort::new(vec![bodies_json()]);
    let report = ingest_file(&root, &raw, &fast, &strong, no_progress())
        .await
        .expect("重试一次后应成功");
    assert!(!report.skipped);
    assert_eq!(fast.call_count(), 2);
    assert!(root.join("wiki/sources").is_dir());
}

/// 失败路径的基线快照：wiki/ 文件树 + 提交数 + git 状态（摄取前 raw 尚
/// 未入库是正常状态，故状态做前后对比而非绝对干净）。
struct FailureBaseline {
    wiki_tree: Vec<(String, String)>,
    commit_count: String,
    status: String,
}

fn failure_baseline(root: &Path) -> FailureBaseline {
    FailureBaseline {
        wiki_tree: tree_snapshot(root, "wiki"),
        commit_count: git(root, &["rev-list", "--count", "HEAD"]),
        status: git(root, &["status", "--porcelain"]),
    }
}

/// 断言失败路径零写入：wiki/ 文件树、提交数、git 状态与摄取前完全一致。
/// （`.corpus/state.db` 指纹缓存被 git 忽略且可再生，不属 Wiki 状态。）
fn assert_failure_clean(root: &Path, baseline: &FailureBaseline) {
    assert_eq!(
        tree_snapshot(root, "wiki"),
        baseline.wiki_tree,
        "失败不得留下任何文件改动"
    );
    assert_eq!(
        git(root, &["rev-list", "--count", "HEAD"]),
        baseline.commit_count,
        "失败不得产生提交"
    );
    assert_eq!(
        git(root, &["status", "--porcelain"]),
        baseline.status,
        "失败后 git 工作区应回到摄取前状态"
    );
}

// ---------------------------------------------------------------------------
// 精确名去重
// ---------------------------------------------------------------------------

/// 验收：同名（不区分大小写 / 别名命中）第二次摄取更新既有页面而非新建。
#[tokio::test]
async fn exact_name_dedup_updates_existing_page() {
    let (_dir, root) = setup_vault("vault");
    let raw_a = write_raw(&root, "a.md", "第一篇：介绍 Transformer。");
    ingest_ok(&root, &raw_a).await;

    let entity_dir = root.join("wiki/entities");
    let entity_files_after_first: Vec<String> = list_md(&entity_dir);
    assert_eq!(entity_files_after_first.len(), 1);
    let entity_path = entity_dir.join(&entity_files_after_first[0]);
    let (fm_first, _) = parse_page(&fs::read_to_string(&entity_path).unwrap()).unwrap();

    // 第二篇 raw：实体名大小写不同（精确名不区分大小写），并带新别名。
    let analysis_b = r#"{
  "source_title": "BERT 论文",
  "source_summary": "基于 Transformer 编码器的预训练。",
  "entities": [
    {"title": "transformer", "aliases": ["vanilla Transformer"], "tags": ["dl"],
     "summary": "补充编码器-解码器细节。", "related": ["自注意力"]}
  ],
  "concepts": []
}"#;
    let bodies_b = r#"{"pages": [
      {"path": "sources/bert-论文", "body": "BERT 基于 [[entities/transformer|transformer]]。"},
      {"path": "entities/transformer", "body": "更新后的 Transformer 正文。"}
    ]}"#;
    let raw_b = write_raw(&root, "b.md", "第二篇：BERT。");
    let fast = ScriptedLlmPort::new(vec![analysis_b.to_string()]);
    let strong = ScriptedLlmPort::new(vec![bodies_b.to_string()]);
    let report = ingest_file(&root, &raw_b, &fast, &strong, no_progress())
        .await
        .expect("第二次摄取应成功");
    assert!(!report.skipped);

    // 实体目录仍只有一个文件：更新而非新建
    let entity_files_after_second = list_md(&entity_dir);
    assert_eq!(
        entity_files_after_second, entity_files_after_first,
        "精确名命中必须更新既有页面，不得新建"
    );
    let updated = report
        .pages
        .iter()
        .find(|p| p.page_type == PageKind::Entity)
        .expect("报告应含实体页");
    assert_eq!(updated.action, PageAction::Update);
    assert_eq!(
        updated.path,
        format!("entities/{}", entity_files_after_first[0])
    );

    // 字段合并：created 保留、aliases 并集（含新别名）、sources 指回两个 Source page
    let (fm_second, body_second) = parse_page(&fs::read_to_string(&entity_path).unwrap()).unwrap();
    assert_eq!(fm_second.created, fm_first.created, "更新保留 created");
    assert!(fm_second.aliases.contains(&"Transformer 架构".to_string()));
    assert!(fm_second
        .aliases
        .contains(&"vanilla Transformer".to_string()));
    assert!(fm_second
        .aliases
        .iter()
        .any(|a| a.eq_ignore_ascii_case("transformer")));
    assert_eq!(
        fm_second.sources,
        vec![
            "[[sources/attention-is-all-you-need]]".to_string(),
            "[[sources/bert-论文]]".to_string(),
        ],
        "sources 应累积两个 Source page"
    );
    assert!(body_second.contains("更新后的 Transformer 正文"));

    // Source page 各自独立（不同 raw）
    assert_eq!(list_md(&root.join("wiki/sources")).len(), 2);
    // git：初始 + 两次摄取 = 3 个提交
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), "3");
}

/// 同一 raw 内容变更后重摄取：更新同一个 Source page（raw 路径锚定）。
#[tokio::test]
async fn same_raw_reingest_after_change_updates_same_source_page() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "第一版内容");
    let first = ingest_ok(&root, &raw).await;

    let raw_changed = write_raw(&root, "a.md", "第二版内容（已修改）");
    assert_eq!(raw_changed, raw);
    let second = ingest_file(
        &root,
        &raw,
        &ScriptedLlmPort::new(vec![analysis_json()]),
        &ScriptedLlmPort::new(vec![bodies_json()]),
        no_progress(),
    )
    .await
    .expect("内容变化后重摄取应成功");
    assert!(!second.skipped, "指纹变化必须重新摄取");
    assert_ne!(second.fingerprint, first.fingerprint);

    // 仍只有一个 Source page，且是更新
    assert_eq!(list_md(&root.join("wiki/sources")).len(), 1);
    assert_eq!(second.pages[0].action, PageAction::Update);
    let (fm, _) = parse_page(
        &fs::read_to_string(root.join("wiki/sources/attention-is-all-you-need.md")).unwrap(),
    )
    .unwrap();
    assert_eq!(fm.fingerprint.as_deref(), Some(second.fingerprint.as_str()));
}

fn list_md(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .expect("列目录失败")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// git 提交 / log / index
// ---------------------------------------------------------------------------

/// 验收：每次 Apply 恰好一个 git 提交；log.md 可 grep；index.md 反映新页。
#[tokio::test]
async fn apply_creates_exactly_one_commit_with_log_and_index() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "paper.md", "内容");
    let report = ingest_ok(&root, &raw).await;

    // 初始提交 + 恰好一个 ingest 提交
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), "2");
    let subject = git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject, "corpus: ingest | Attention Is All You Need");
    assert!(report.commit.is_some());

    // log.md：追加可 grep 的时序记录
    let log = fs::read_to_string(root.join("wiki/log.md")).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert!(
        log.contains(&format!(
            "## [{today}] corpus: ingest | Attention Is All You Need"
        )),
        "log.md 应含 `## [日期] corpus: ingest | 标题` 记录"
    );

    // index.md：占位清除 + 三个新页条目
    let index = fs::read_to_string(root.join("wiki/index.md")).unwrap();
    assert!(!index.contains("（暂无页面）"));
    assert!(index.contains("[[sources/attention-is-all-you-need|Attention Is All You Need]]"));
    assert!(index.contains("[[entities/transformer|Transformer]]"));
    assert!(index.contains("[[concepts/自注意力|自注意力]]"));

    // 提交内容：wiki 页面 + index + log，且不含 state.db
    let files = git(&root, &["show", "--name-only", "--format=", "HEAD"]);
    for expect in [
        "wiki/sources/attention-is-all-you-need.md",
        "wiki/entities/transformer.md",
        "wiki/concepts/自注意力.md",
        "wiki/index.md",
        "wiki/log.md",
    ] {
        assert!(
            files.lines().any(|l| l == expect),
            "提交缺 {expect}：\n{files}"
        );
    }
    assert!(!files.contains("state.db"), "指纹缓存不得入库");
    assert!(
        root.join(".corpus/state.db").is_file(),
        "指纹缓存应存在于本地"
    );

    // 再摄取另一个文件：又是恰好一个提交
    let raw_b = write_raw(&root, "b.md", "第二篇");
    let analysis_b =
        r#"{"source_title": "BERT", "source_summary": "s", "entities": [], "concepts": []}"#;
    let bodies_b = r#"{"pages": [{"path": "sources/bert", "body": "BERT 摘要。"}]}"#;
    ingest_file(
        &root,
        &raw_b,
        &ScriptedLlmPort::new(vec![analysis_b.to_string()]),
        &ScriptedLlmPort::new(vec![bodies_b.to_string()]),
        no_progress(),
    )
    .await
    .expect("第二次摄取失败");
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), "3");
}

// ---------------------------------------------------------------------------
// raw/ 零写入不变量
// ---------------------------------------------------------------------------

/// 验收：raw/ 目录（含 raw 文件本身）摄取前后逐字节不变。
#[tokio::test]
async fn raw_directory_is_never_written() {
    let (_dir, root) = setup_vault("vault");
    write_raw(&root, "paper.md", "论文内容");
    let before = tree_snapshot(&root, "raw");

    ingest_ok(&root, "raw/paper.md").await;

    // 成功摄取也不改 raw/ 一个字节
    assert_eq!(tree_snapshot(&root, "raw"), before);
    // 失败路径同样不动 raw/
    let raw_b = write_raw(&root, "bad.md", "将触发失败的内容");
    let after_first = tree_snapshot(&root, "raw");
    let fast = ScriptedLlmPort::new(vec!["坏输出".to_string(), "还是坏".to_string()]);
    let strong = ScriptedLlmPort::new(vec![]);
    assert!(ingest_file(&root, &raw_b, &fast, &strong, no_progress())
        .await
        .is_err());
    assert_eq!(tree_snapshot(&root, "raw"), after_first);
    let _ = before;
}

// ---------------------------------------------------------------------------
// wikilink 回读（Obsidian 兼容性验收）
// ---------------------------------------------------------------------------

/// percent-decode（pulldown-cmark 的 dest_url 可能对 CJK 做 URL 编码）。
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 < bytes.len() {
            let hex = &input[i + 1..i + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 验收：生成页面用 pulldown-cmark ENABLE_WIKILINKS 回读，全部 wikilink
/// 有效——每个目标都对应 Wiki 中真实存在的页面文件。
#[tokio::test]
async fn wikilinks_round_trip_valid_via_pulldown() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "paper.md", "内容");
    let report = ingest_ok(&root, &raw).await;

    // Wiki 现存页面 link 集（相对 wiki/ 去 .md）
    let mut existing_links: Vec<String> = Vec::new();
    for page in &report.pages {
        existing_links.push(page.path.trim_end_matches(".md").to_string());
    }

    use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_WIKILINKS);

    let mut collected: Vec<String> = Vec::new();
    for page in &report.pages {
        let content = fs::read_to_string(root.join("wiki").join(&page.path)).unwrap();
        let (fm, body) = parse_page(&content).unwrap();
        for event in Parser::new_ext(&body, opts) {
            if let Event::Start(Tag::Link {
                link_type: LinkType::WikiLink { .. },
                dest_url,
                ..
            }) = event
            {
                let target = percent_decode(&dest_url);
                assert!(
                    existing_links.contains(&target),
                    "{} 中的 wikilink [[{target}]] 未指向任何产出页面",
                    page.path
                );
                collected.push(target);
            }
        }
        // frontmatter 里的 related/sources 也是 wikilink，同样必须有效
        for link in fm.related.iter().chain(fm.sources.iter()) {
            let target = link.trim_start_matches("[[").trim_end_matches(']');
            let target = target.split('|').next().unwrap_or(target);
            assert!(
                existing_links.contains(&target.to_string()),
                "{} frontmatter 链接 {link} 无效",
                page.path
            );
        }
    }
    assert!(
        collected.len() >= 3,
        "回读至少应收集到脚本写入的三个 wikilink，实际 {collected:?}"
    );
}

// ---------------------------------------------------------------------------
// 输入校验
// ---------------------------------------------------------------------------

#[tokio::test]
async fn raw_outside_vault_is_rejected() {
    let (_dir, root) = setup_vault("vault");
    let (other_dir, other_root) = fresh_vault_path("outside");
    fs::create_dir_all(&other_root).unwrap();
    fs::write(other_root.join("外部文件.md"), "内容").unwrap();

    let (fast, strong) = standard_llms();
    let err = ingest_file(&root, &other_root, &fast, &strong, no_progress())
        .await
        .expect_err("Vault 外文件必须拒绝");
    assert!(matches!(err, Error::RawNotInVault { .. }), "got: {err:?}");
    assert_eq!(fast.call_count(), 0);
    let _ = other_dir;
}

#[tokio::test]
async fn non_vault_directory_is_rejected() {
    let (dir, root) = fresh_vault_path("not-a-vault");
    fs::create_dir_all(&root).unwrap();
    let (fast, strong) = standard_llms();
    let err = ingest_file(&root, "raw/a.md", &fast, &strong, no_progress())
        .await
        .expect_err("非 Vault 目录必须拒绝");
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
    let _ = dir;
}
