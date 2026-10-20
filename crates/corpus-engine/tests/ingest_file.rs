//! ingest_file 编排层集成测试（票据 #17 v1 的下沉验收）：编排原为桌面
//! 壳的 Tauri 命令逻辑，下沉引擎后在此经公共 API 离线验证。
//!
//! 端口注入走 `ingest_file_with_ports`（编排函数的显式端口参数，改动
//! 最小的测试缝——见 ingest.rs 文档）：Scripted Fake 走通 auto 全链，
//! 并验证 manual 分支零写入语义。真实 rig 端口变体（`ingest_file`）仅
//! 在 Fake 之上多一步「按档位构造端口」，无独立逻辑。

mod common;

use std::fs;

use common::{fresh_vault_path, git, tree_snapshot, ScriptedLlmPort};
use corpus_engine::config::{AppConfig, ApplyMode, ModelTier, ProviderKind};
use corpus_engine::{
    create_vault, ingest_file_with_ports, IngestFileReport, IngestStage, TemplateId,
    MANUAL_REVIEW_NOTE,
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

/// Fake 档位：编排层只透传给端口注入缝，模型配置内容不参与管道行为。
fn fake_tier() -> ModelTier {
    ModelTier {
        provider: ProviderKind::OpenAI,
        model: "fake-model".to_string(),
        base_url: None,
        api_key_env: Some("CORPUS_INTEGRATION_TEST_UNSET_KEY".to_string()),
    }
}

fn config_with(apply_mode: ApplyMode) -> AppConfig {
    AppConfig {
        fast: fake_tier(),
        strong: fake_tier(),
        apply_mode,
        query_pages: true,
    }
}

/// 第一步（fast 档）的标准分析输出（与 tests/ingest.rs 同一脚本）。
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

/// 第二步（strong 档）的标准成文输出（与 tests/ingest.rs 同一脚本）。
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

fn standard_llms() -> (ScriptedLlmPort, ScriptedLlmPort) {
    (
        ScriptedLlmPort::new(vec![analysis_json()]),
        ScriptedLlmPort::new(vec![bodies_json()]),
    )
}

fn no_progress() -> impl FnMut(IngestStage) + Send {
    |_| {}
}

// ---------------------------------------------------------------------------
// auto 全链：外部文件 → 导入 → 摄取 → 自动落盘
// ---------------------------------------------------------------------------

/// 验收（编排层 auto 全链）：Vault 外源文件经导入段复制进 raw/，管道
/// 摄取后按 apply_mode=auto 自动落盘——报告带 applied=true / 导入信息 /
/// commit，Wiki 真实落文件，恰好一个 ingest 提交，源文件不动。
#[tokio::test]
async fn ingest_file_auto_full_chain_from_external_source() {
    let (_dir, root) = setup_vault("vault");
    let (ext_dir, ext_root) = fresh_vault_path("external-source");
    fs::create_dir_all(&ext_root).expect("建外部目录失败");
    let src = ext_root.join("paper.md");
    fs::write(&src, "论文正文……").expect("写外部源文件失败");

    let (fast, strong) = standard_llms();
    let report: IngestFileReport = ingest_file_with_ports(
        &root,
        &src,
        &config_with(ApplyMode::Auto),
        &fast,
        &strong,
        no_progress(),
    )
    .await
    .expect("编排应成功");

    // 导入段：新导入，相对路径正确
    assert!(report.newly_imported);
    assert_eq!(report.imported_raw_path, "raw/paper.md");
    assert!(root.join("raw/paper.md").is_file(), "raw/ 应有导入副本");
    assert!(src.is_file(), "导入是复制而非移动，源文件不动");

    // Apply 段：auto 模式自动落盘
    assert!(!report.report.skipped);
    assert!(report.applied, "auto 模式必须已落盘");
    assert_eq!(report.note, None);
    assert!(report.summary_lines.is_empty());
    assert!(report.report.commit.is_some());
    assert_eq!(report.report.pages.len(), 3);

    // 真实落盘与 git：初始提交 + 恰好一个 ingest 提交
    assert!(root
        .join("wiki/sources/attention-is-all-you-need.md")
        .is_file());
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), "2");
    let _ = ext_dir;

    // 重复摄取同一文件：导入去重（不再新导入）+ 指纹跳过（零 LLM 调用）
    let (fast, strong) = standard_llms();
    let second = ingest_file_with_ports(
        &root,
        &src,
        &config_with(ApplyMode::Auto),
        &fast,
        &strong,
        no_progress(),
    )
    .await
    .expect("重复编排应成功（跳过路径）");
    assert!(second.report.skipped, "指纹未变必须跳过");
    assert!(!second.newly_imported, "同内容导入去重命中既有文件");
    assert!(!second.applied, "跳过路径未落盘");
    assert_eq!(second.imported_raw_path, "raw/paper.md");
    assert_eq!(fast.call_count(), 0, "跳过路径零 LLM 调用");
    assert_eq!(strong.call_count(), 0);
}

// ---------------------------------------------------------------------------
// manual 分支：不落盘语义原样保留
// ---------------------------------------------------------------------------

/// 验收（编排层 manual 分支）：Draft 不落盘不提交、指纹不记录，报告带
/// applied=false / 检视清单 / MANUAL_REVIEW_NOTE 注明。
#[tokio::test]
async fn ingest_file_manual_branch_writes_nothing() {
    let (_dir, root) = setup_vault("vault");
    let raw_rel = "raw/manual.md";
    fs::create_dir_all(root.join("raw")).expect("建 raw 失败");
    fs::write(root.join(raw_rel), "论文正文……").expect("写 raw 失败");

    let baseline = tree_snapshot(&root, "wiki");
    let commits_before = git(&root, &["rev-list", "--count", "HEAD"]);
    let status_before = git(&root, &["status", "--porcelain"]);

    let (fast, strong) = standard_llms();
    let report = ingest_file_with_ports(
        &root,
        raw_rel,
        &config_with(ApplyMode::Manual),
        &fast,
        &strong,
        no_progress(),
    )
    .await
    .expect("编排应成功");

    // 报告语义：Draft 交还检视，未落盘
    assert!(!report.report.skipped);
    assert!(!report.applied, "manual 模式不得落盘");
    assert!(report.report.commit.is_none());
    assert_eq!(report.note.as_deref(), Some(MANUAL_REVIEW_NOTE));
    assert!(
        report.summary_lines.len() >= 2,
        "检视清单应含 source 行 + 每页一行：{:?}",
        report.summary_lines
    );
    assert!(report.imported_raw_path == raw_rel && !report.newly_imported);

    // 零写入：wiki/ 文件树、提交数、git 状态与摄取前完全一致
    assert_eq!(
        tree_snapshot(&root, "wiki"),
        baseline,
        "manual 不得改 wiki/"
    );
    assert_eq!(git(&root, &["rev-list", "--count", "HEAD"]), commits_before);
    assert_eq!(git(&root, &["status", "--porcelain"]), status_before);

    // 指纹未记录：同一 raw 再次编排仍走 LLM（不跳过）
    let (fast, strong) = standard_llms();
    ingest_file_with_ports(
        &root,
        raw_rel,
        &config_with(ApplyMode::Manual),
        &fast,
        &strong,
        no_progress(),
    )
    .await
    .expect("第二次编排应重新走管道");
    assert_eq!(fast.call_count(), 1, "未 apply → 未记录指纹 → 不跳过");
}

// ---------------------------------------------------------------------------
// 输入校验透传
// ---------------------------------------------------------------------------

/// 验收：非 Vault 目录在编排第一段（import_raw 的 Vault 校验）即被拒，
/// 零 LLM 调用、零写入（编排层不放宽引擎既有契约）。
#[tokio::test]
async fn ingest_file_rejects_non_vault_root() {
    let (dir, not_a_vault) = fresh_vault_path("not-a-vault");
    fs::create_dir_all(&not_a_vault).expect("建目录失败");
    let src = not_a_vault.join("paper.md");
    fs::write(&src, "内容").expect("写源文件失败");

    let (fast, strong) = standard_llms();
    let err = ingest_file_with_ports(
        &not_a_vault,
        &src,
        &config_with(ApplyMode::Auto),
        &fast,
        &strong,
        no_progress(),
    )
    .await
    .expect_err("非 Vault 目录必须被拒绝");
    assert!(
        matches!(err, corpus_engine::Error::NotAVault { .. }),
        "got: {err:?}"
    );
    assert_eq!(fast.call_count(), 0, "导入失败不得进入管道");
    assert_eq!(strong.call_count(), 0);
    let _ = dir;
}
