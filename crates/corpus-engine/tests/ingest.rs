//! Ingest 集成测试（票据 T3 验收）：只经引擎公共 API 驱动，LLM 接缝用
//! Scripted Fake（离线、确定性），断言落在真实文件系统（页面内容、
//! frontmatter schema、git 历史、raw/ 不变量）与 wikilink 回读。
//! 两段式 API：`ingest`（→ Draft，不落盘）+ 显式 `apply`。

mod common;

use std::fs;
use std::path::Path;

use common::{fresh_vault_path, git, tree_snapshot, ScriptedLlmPort, StreamingLlmPort};
use corpus_engine::{
    apply, create_vault, ingest, parse_page, Error, IngestDraft, IngestOutcome, IngestReport,
    IngestStage, PageAction, PageKind, TemplateId,
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

/// 从 ingest 结果取 Draft（跳过则 panic：调用方断言非跳过路径）。
fn draft_of(outcome: IngestOutcome) -> IngestDraft {
    match outcome {
        IngestOutcome::Draft(draft) => draft,
        IngestOutcome::Skipped(report) => panic!("预期产出 Draft，实际指纹跳过：{report:?}"),
    }
}

/// 第二段：显式 apply → 报告（含 commit）。
fn apply_ok(root: &Path, draft: &IngestDraft) -> IngestReport {
    let commit = apply(root, draft, no_progress()).expect("apply 失败");
    draft.report(Some(commit))
}

/// 第一步（标准脚本）：ingest → Draft，不落盘。
async fn standard_draft(root: &Path, raw_rel: &str) -> IngestDraft {
    let (fast, strong) = standard_llms();
    draft_of(
        ingest(root, raw_rel, &fast, &strong, no_progress())
            .await
            .expect("标准脚本 ingest 应成功"),
    )
}

/// 两段式完整摄取（标准脚本）：ingest → Draft → apply → 报告。
async fn ingest_ok(root: &Path, raw_rel: &str) -> IngestReport {
    let draft = standard_draft(root, raw_rel).await;
    apply_ok(root, &draft)
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
    let second = match ingest(&root, &raw, &fast, &strong, no_progress())
        .await
        .expect("重复摄取应成功返回（跳过路径）")
    {
        IngestOutcome::Skipped(report) => report,
        IngestOutcome::Draft(_) => panic!("指纹未变必须跳过，不得产出 Draft"),
    };
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
/// fast 的 system prompt 注入模板 schema_prompt；两段进度阶段有序
/// （ingest 三段 + apply 落盘段）。
#[tokio::test]
async fn two_llm_steps_each_once_with_correct_tier_and_schema_prompt() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");

    let (fast, strong) = standard_llms();
    let mut stages: Vec<String> = Vec::new();
    let stage_name = |stage: &IngestStage| {
        serde_json::to_value(stage).unwrap()["stage"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let outcome = ingest(&root, &raw, &fast, &strong, |stage| {
        stages.push(stage_name(&stage));
    })
    .await
    .expect("摄取失败");
    let draft = draft_of(outcome);

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

    // 第二段 apply：落盘进度
    apply(&root, &draft, |stage| stages.push(stage_name(&stage))).expect("apply 失败");

    // 进度阶段：读取 → 分析 → 生成 → 落盘（两段合计，依序且齐全）。
    // 分析/生成会因流式 chars 上报重复出现（节流合并后仍可能多报），
    // 折叠相邻同名再断言顺序。
    let collapsed: Vec<String> = stages
        .into_iter()
        .fold(Vec::new(), |mut acc: Vec<String>, s| {
            if acc.last().map(|last| last != &s).unwrap_or(true) {
                acc.push(s);
            }
            acc
        });
    assert_eq!(
        collapsed,
        vec!["reading", "analyzing", "generating", "applying"]
    );
}

// ---------------------------------------------------------------------------
// 流式进度透明度（chars 增量 + 节流）
// ---------------------------------------------------------------------------

/// 带进度记录的摄取助手：返回（分析 chars 序列、生成 (pages, chars)
/// 序列）。Draft 只取不落盘（进度断言与落盘无关）。
async fn run_with_progress<F, S>(
    root: &Path,
    raw_rel: &str,
    fast: F,
    strong: S,
) -> (Vec<usize>, Vec<(usize, usize)>)
where
    F: corpus_engine::LlmPort,
    S: corpus_engine::LlmPort,
{
    let mut analyzing: Vec<usize> = Vec::new();
    let mut generating: Vec<(usize, usize)> = Vec::new();
    draft_of(
        ingest(root, raw_rel, &fast, &strong, |stage| match stage {
            IngestStage::Analyzing { chars } => analyzing.push(chars),
            IngestStage::Generating { pages, chars } => generating.push((pages, chars)),
            _ => {}
        })
        .await
        .expect("摄取失败"),
    );
    (analyzing, generating)
}

/// chars 事件断言：首事件 0（阶段开始）、末事件到达总字数、严格递增、
/// 连续小增量被节流合并（约每 50 字一报，远少于 10 字/段的分段总数）。
fn assert_chars_progress(chars: &[usize], total: usize, what: &str) {
    assert!(!chars.is_empty(), "{what}阶段应有 chars 事件");
    assert_eq!(chars[0], 0, "{what}首事件应为阶段开始（chars=0）");
    assert_eq!(*chars.last().unwrap(), total, "{what}末事件应到达总字数");
    assert!(
        chars.windows(2).all(|w| w[0] < w[1]),
        "{what}chars 应严格递增：{chars:?}"
    );
    let chunks = total.div_ceil(10);
    assert!(
        chars.len() <= total / 50 + 2,
        "{what}连续小增量（{chunks} 段）应合并成约每 50 字一报：{chars:?}"
    );
}

/// 验收：流式 fake 分段吐增量（10 字/段、零间隔）时，分析/生成两步的
/// 进度事件携带递增 chars 且节流生效（小增量合并）；生成事件保持
/// pages 字段。
#[tokio::test]
async fn streamed_progress_reports_throttled_increasing_chars() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");
    // 10 字/段、零间隔：时间阈值（200ms）不会触发，只走字符阈值（50 字）
    let fast = StreamingLlmPort::over(vec![analysis_json()], 10, 0);
    let strong = StreamingLlmPort::over(vec![bodies_json()], 10, 0);
    let (analyzing, generating) = run_with_progress(&root, &raw, fast, strong).await;

    assert_chars_progress(&analyzing, analysis_json().chars().count(), "分析");
    assert_chars_progress(
        &generating.iter().map(|(_, c)| *c).collect::<Vec<_>>(),
        bodies_json().chars().count(),
        "生成",
    );
    // 生成事件携带计划页面数（标准脚本：1 Source + 1 Entity + 1 Concept）
    assert!(
        generating.iter().all(|(pages, _)| *pages == 3),
        "生成事件应携带 pages=3：{generating:?}"
    );
}

/// 验收：只实现 complete 的端口走 trait 默认回退——整体结果作为单次
/// 增量，chars 从 0 直达总字数（恰好一次上报）。
#[tokio::test]
async fn default_fallback_port_reports_single_chars_update() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");
    let fast = ScriptedLlmPort::new(vec![analysis_json()]);
    let strong = ScriptedLlmPort::new(vec![bodies_json()]);
    let (analyzing, generating) = run_with_progress(&root, &raw, fast, strong).await;
    assert_eq!(analyzing, vec![0, analysis_json().chars().count()]);
    assert_eq!(generating, vec![(3, 0), (3, bodies_json().chars().count())]);
}

/// 验收：时间阈值路径——分段间隔 250ms（> 200ms 阈值）时，小增量
/// （15 字/段，达不到 50 字字符阈值）逐段上报。
#[tokio::test]
async fn slow_stream_reports_each_delta_via_time_threshold() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");
    let analysis = r#"{"source_title":"T","source_summary":"s","entities":[],"concepts":[]}"#;
    let total = analysis.chars().count();
    let fast = StreamingLlmPort::over(vec![analysis.to_string()], 15, 250);
    // strong 不设间隔、整段吐出：生成阶段与本断言无关
    let strong = StreamingLlmPort::over(vec![bodies_json()], 10_000, 0);
    let (analyzing, _generating) = run_with_progress(&root, &raw, fast, strong).await;

    let mut expected = vec![0usize];
    let mut acc = 0usize;
    while acc < total {
        acc = (acc + 15).min(total);
        expected.push(acc);
    }
    assert_eq!(
        analyzing, expected,
        "间隔超过时间阈值的每个小增量都应上报（不受 50 字阈值限制）"
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
    let err = ingest(&root, &raw, &fast, &strong, no_progress())
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
    let err = ingest(&root, &raw, &fast, &strong, no_progress())
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
    let draft = draft_of(
        ingest(&root, &raw, &fast, &strong, no_progress())
            .await
            .expect("重试一次后应成功"),
    );
    assert_eq!(fast.call_count(), 2);
    apply(&root, &draft, no_progress()).expect("apply 失败");
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
// 人审模式语义（ingest 不 apply）
// ---------------------------------------------------------------------------

/// 验收（apply_mode=manual 的引擎级语义）：只 ingest 不 apply 时零写入、
/// 零提交、指纹不记录——Draft 是纯内存变更集，丢弃后 Wiki 与摄取前一致，
/// 重摄取会重新走 LLM（跳过仅在 apply 成功记录指纹后发生）。
#[tokio::test]
async fn draft_without_apply_writes_nothing() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "内容");
    let state = failure_baseline(&root);

    let draft = standard_draft(&root, &raw).await;
    // Draft 可整体检视（人读清单：source 行 + 每页一行）
    assert!(draft.summary_lines().len() >= 2);
    assert_failure_clean(&root, &state);

    // 指纹未记录：同一 raw 再次 ingest 不走跳过路径（重新调用 LLM）
    let (fast, strong) = standard_llms();
    draft_of(
        ingest(&root, &raw, &fast, &strong, no_progress())
            .await
            .expect("未 apply 则指纹未记录，重摄取应重新分析"),
    );
    assert_eq!(fast.call_count(), 1, "未 apply → 未记录指纹 → 不跳过");
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
    let report = apply_ok(
        &root,
        &draft_of(
            ingest(&root, &raw_b, &fast, &strong, no_progress())
                .await
                .expect("第二次摄取应成功"),
        ),
    );

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
    let second = apply_ok(
        &root,
        &draft_of(
            ingest(
                &root,
                &raw,
                &ScriptedLlmPort::new(vec![analysis_json()]),
                &ScriptedLlmPort::new(vec![bodies_json()]),
                no_progress(),
            )
            .await
            .expect("内容变化后重摄取应成功"),
        ),
    );
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
// related 解析：unresolved 死链保留
// ---------------------------------------------------------------------------

/// 验收：related 指向不存在的页面时原样保留为短形式 unresolved wikilink
/// （死链合法，目标页产出后 Obsidian 自动解析；死链检测属 T11 lint），
/// 可解析项照常解析为路径形式。
#[tokio::test]
async fn unresolved_related_is_preserved_as_dead_link() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "内容");
    let analysis = r#"{
  "source_title": "T",
  "source_summary": "s",
  "entities": [
    {"title": "Transformer", "aliases": [], "tags": [],
     "summary": "架构。", "related": ["自注意力", "不存在的幽灵页面"]}
  ],
  "concepts": [
    {"title": "自注意力", "aliases": [], "tags": [], "summary": "机制。", "related": []}
  ]
}"#;
    let bodies = r#"{"pages": [
      {"path": "sources/t", "body": "b"},
      {"path": "entities/transformer", "body": "b"},
      {"path": "concepts/自注意力", "body": "b"}
    ]}"#;
    let draft = draft_of(
        ingest(
            &root,
            &raw,
            &ScriptedLlmPort::new(vec![analysis.to_string()]),
            &ScriptedLlmPort::new(vec![bodies.to_string()]),
            no_progress(),
        )
        .await
        .expect("摄取失败"),
    );
    apply(&root, &draft, no_progress()).expect("apply 失败");

    let (fm, _) =
        parse_page(&fs::read_to_string(root.join("wiki/entities/transformer.md")).unwrap())
            .unwrap();
    assert_eq!(
        fm.related,
        vec![
            "[[concepts/自注意力]]".to_string(),
            "[[不存在的幽灵页面]]".to_string(),
        ],
        "可解析项 → 路径形式；不可解析项原样保留为短形式死链"
    );
}

// ---------------------------------------------------------------------------
// 漏页降级：摘要兜底的显式标记
// ---------------------------------------------------------------------------

/// 验收：第二步漏产出某页正文时管道继续，页面以摘要兜底，且 Source
/// 产出索引与 Draft 检视清单都带显式降级标注（可 grep）。
#[tokio::test]
async fn missing_body_degrades_with_explicit_marker() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "paper.md", "内容");
    // 成文脚本漏掉 concepts/自注意力
    let bodies = r#"{"pages": [
      {"path": "sources/attention-is-all-you-need", "body": "摘要正文。"},
      {"path": "entities/transformer", "body": "实体正文。"}
    ]}"#;
    let fast = ScriptedLlmPort::new(vec![analysis_json()]);
    let strong = ScriptedLlmPort::new(vec![bodies.to_string()]);
    let draft = draft_of(
        ingest(&root, &raw, &fast, &strong, no_progress())
            .await
            .expect("ingest 失败"),
    );

    // Draft 检视清单：降级页带标注
    let degraded_line = draft
        .summary_lines()
        .into_iter()
        .find(|l| l.contains("concepts/自注意力"))
        .expect("清单应含概念页行");
    assert!(
        degraded_line.contains("降级：模型未产出，摘要兜底"),
        "{degraded_line}"
    );

    apply(&root, &draft, no_progress()).expect("apply 失败");
    // 降级页正文：显式声明 + 分析摘要兜底
    let (_, concept_body) =
        parse_page(&fs::read_to_string(root.join("wiki/concepts/自注意力.md")).unwrap()).unwrap();
    assert!(concept_body.contains("（本页正文暂缺，以分析摘要代替）"));
    assert!(concept_body.contains("让序列中每个位置直接关注全部位置的机制"));

    // Source 产出索引：降级条目带标注，正常条目不带
    let (_, source_body) = parse_page(
        &fs::read_to_string(root.join("wiki/sources/attention-is-all-you-need.md")).unwrap(),
    )
    .unwrap();
    assert!(
        source_body
            .contains("[[concepts/自注意力|自注意力]]（新建）（降级：模型未产出，摘要兜底）"),
        "降级条目应带显式标注：{source_body}"
    );
    assert!(
        source_body.contains("[[entities/transformer|Transformer]]（新建）\n"),
        "正常条目不应带标注：{source_body}"
    );
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

    // log.md：追加可 grep 的时序记录（规格格式，无提交信息前缀）
    let log = fs::read_to_string(root.join("wiki/log.md")).unwrap();
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert!(
        log.contains(&format!("## [{today}] ingest | Attention Is All You Need")),
        "log.md 应含 `## [日期] ingest | 标题` 记录：{log}"
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
    apply_ok(
        &root,
        &draft_of(
            ingest(
                &root,
                &raw_b,
                &ScriptedLlmPort::new(vec![analysis_b.to_string()]),
                &ScriptedLlmPort::new(vec![bodies_b.to_string()]),
                no_progress(),
            )
            .await
            .expect("第二次摄取失败"),
        ),
    );
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), "3");
}

/// 验收（apply 失败分支）：git commit 失败时恢复全部文件快照并清空
/// 暂存区——工作区与索引都回到提交点，不留「已暂存但工作区已恢复」的
/// 幽灵条目。用必定失败的 pre-commit 钩子迫使 commit 失败（add 仍成功，
/// 精确命中「add 之后 commit 失败」分支；chmod .git 会让 add 一起失败，
/// 无法覆盖该分支，故弃用）。仓库级 `core.hooksPath` 显式指回
/// `.git/hooks`，不受开发机全局 hooksPath 覆盖（如 CCM）影响。
#[tokio::test]
#[cfg(unix)]
async fn apply_commit_failure_restores_worktree_and_clears_index() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "paper.md", "内容");
    let state = failure_baseline(&root);

    git(&root, &["config", "core.hooksPath", ".git/hooks"]);
    let hook = root.join(".git/hooks/pre-commit");
    fs::write(&hook, "#!/bin/sh\nexit 1\n").expect("写 pre-commit 钩子失败");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).expect("设钩子执行位失败");

    let draft = standard_draft(&root, &raw).await;
    let err = apply(&root, &draft, no_progress()).expect_err("commit 钩子失败必须报错");
    assert!(matches!(err, Error::Git { .. }), "got: {err:?}");
    // 工作区与暂存区都回到提交点（无 reset 时 status 会残留已暂存条目）
    assert_failure_clean(&root, &state);
    // 指纹未记录：失败的 raw 可直接重摄取
    let reingest = standard_draft(&root, &raw).await;
    assert_eq!(reingest.pages.len(), draft.pages.len());
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
    assert!(ingest(&root, &raw_b, &fast, &strong, no_progress())
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
    let err = ingest(&root, &other_root, &fast, &strong, no_progress())
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
    let err = ingest(&root, "raw/a.md", &fast, &strong, no_progress())
        .await
        .expect_err("非 Vault 目录必须拒绝");
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
    let _ = dir;
}

// ---------------------------------------------------------------------------
// LLM 输出治理：熔断 / 重试约束 / 调用统计与日志
// ---------------------------------------------------------------------------

/// 造一个超过分析熔断上限（30,000 字符）的分析输出（形状仍是 JSON——
/// 熔断先于解析，形状无所谓，但保持 JSON 以证明「超限不看内容」）。
fn oversized_analysis(extra: usize) -> String {
    format!(
        r#"{{"source_title":"T","source_summary":"{}","entities":[],"concepts":[]}}"#,
        "长".repeat(30_000 + extra)
    )
}

/// 读 Vault 的 llm-calls.log 全部行（文件须存在）。
fn llm_calls_log(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join(".corpus/llm-calls.log"))
        .expect("llm-calls.log 应存在")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// 验收：分析输出超熔断上限 → 该次按 Oversized 失败（不进解析），
/// 重试的 user 消息追加强约束句，第二次正常输出则整单成功。
#[tokio::test]
async fn oversized_output_trips_breaker_then_retry_succeeds() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");
    let fast = StreamingLlmPort::over(vec![oversized_analysis(10), analysis_json()], 10_000, 0);
    let strong = StreamingLlmPort::over(vec![bodies_json()], 10_000, 0);
    let draft = draft_of(
        ingest(&root, &raw, &fast, &strong, no_progress())
            .await
            .expect("第二次输出正常，整单应成功"),
    );

    let analyze_stats: Vec<_> = draft
        .llm_calls
        .iter()
        .filter(|s| s.purpose == corpus_engine::LlmCallPurpose::Analyze)
        .collect();
    assert_eq!(
        analyze_stats.len(),
        2,
        "分析应恰好两次调用：{:?}",
        draft.llm_calls
    );
    assert_eq!(
        analyze_stats[0].outcome,
        corpus_engine::LlmCallOutcome::Oversized
    );
    assert!(analyze_stats[0]
        .error
        .as_deref()
        .unwrap_or("")
        .contains("超过上限"));
    assert!(analyze_stats[0].output_chars > 30_000);
    assert_eq!(analyze_stats[1].outcome, corpus_engine::LlmCallOutcome::Ok);

    // 重试的 user 消息追加强约束
    let calls = fast.recorded_calls();
    assert_eq!(calls.len(), 2);
    assert!(
        calls[1].1.contains("禁止复述原文"),
        "重试 user 应含强约束：{}",
        calls[1].1
    );
    assert!(!calls[0].1.contains("禁止复述原文"), "首次尝试不应有约束句");

    // 日志：两行 JSON、结果分别是 oversized / ok
    let log = llm_calls_log(&root);
    assert!(log.len() >= 2, "日志应至少记录两次分析调用：{log:?}");
    assert!(log[0].contains("\"oversized\"") && log[1].contains("\"ok\""));
}

/// 验收：两次输出均超限 → BadLlmOutput，错误含两次输出字数；
/// 日志仍留下两条 Oversized 记录（摄取失败日志不丢）。
#[tokio::test]
async fn both_attempts_oversized_reports_char_counts() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");
    let fast = StreamingLlmPort::over(
        vec![oversized_analysis(100), oversized_analysis(50)],
        10_000,
        0,
    );
    let strong = StreamingLlmPort::over(vec![bodies_json()], 10_000, 0);
    let err = ingest(&root, &raw, &fast, &strong, no_progress())
        .await
        .expect_err("两次超限应报错");
    let msg = err.to_string();
    assert!(msg.contains("超过上限"), "错误应含上限说明：{msg}");
    let first = oversized_analysis(100).chars().count();
    let second = oversized_analysis(50).chars().count();
    assert!(
        msg.contains(&first.to_string()) && msg.contains(&second.to_string()),
        "错误应含两次输出字数（{first}/{second}）：{msg}"
    );

    let log = llm_calls_log(&root);
    assert_eq!(log.len(), 2, "失败路径日志不丢：{log:?}");
    assert!(log.iter().all(|l| l.contains("\"oversized\"")));
}

/// 验收：解析失败（非超限）重试同样追加强约束句，统计记录 ParseFailed。
#[tokio::test]
async fn parse_failure_retry_appends_constraint_and_stats() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "论文正文内容");
    let fast = StreamingLlmPort::over(vec!["这不是 JSON".to_string(), analysis_json()], 10_000, 0);
    let strong = StreamingLlmPort::over(vec![bodies_json()], 10_000, 0);
    let draft = draft_of(
        ingest(&root, &raw, &fast, &strong, no_progress())
            .await
            .expect("第二次解析成功，整单应成功"),
    );
    let analyze_stats: Vec<_> = draft
        .llm_calls
        .iter()
        .filter(|s| s.purpose == corpus_engine::LlmCallPurpose::Analyze)
        .collect();
    assert_eq!(
        analyze_stats[0].outcome,
        corpus_engine::LlmCallOutcome::ParseFailed
    );
    assert_eq!(analyze_stats[1].outcome, corpus_engine::LlmCallOutcome::Ok);
    assert!(fast.recorded_calls()[1].1.contains("禁止复述原文"));
}

/// 验收：标准两步成功时统计齐全（fast/analyze + strong/compose、ok、
/// 字数耗时 > 0），日志逐行可解析、且不含 raw 原文（隐私：只有长度）。
#[tokio::test]
async fn llm_calls_log_records_all_steps_without_raw_content() {
    let (_dir, root) = setup_vault("vault");
    let raw = write_raw(&root, "a.md", "RAW_MARKER_XYZ 唯一标记的正文内容");
    let report = ingest_ok(&root, &raw).await;

    assert_eq!(
        report.llm_calls.len(),
        2,
        "两步各一次：{:?}",
        report.llm_calls
    );
    assert_eq!(report.llm_calls[0].tier, corpus_engine::LlmTier::Fast);
    assert_eq!(report.llm_calls[1].tier, corpus_engine::LlmTier::Strong);
    assert!(report
        .llm_calls
        .iter()
        .all(|s| s.outcome == corpus_engine::LlmCallOutcome::Ok));
    assert!(report.llm_calls.iter().all(|s| s.output_chars > 0));
    // user_chars 随文档进入 prompt（必然包含标记）——但日志行绝不能含原文
    assert!(report.llm_calls[0].user_chars > 0);

    let log = llm_calls_log(&root);
    assert_eq!(log.len(), 2, "恰好两行：{log:?}");
    for line in &log {
        let v: serde_json::Value = serde_json::from_str(line).expect("每行应是合法 JSON");
        assert!(v["tier"].is_string() && v["purpose"].is_string() && v["output_chars"].is_u64());
        assert!(
            !line.contains("RAW_MARKER_XYZ"),
            "日志不得包含 raw 原文：{line}"
        );
    }
}
