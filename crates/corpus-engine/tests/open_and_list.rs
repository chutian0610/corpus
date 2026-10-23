//! open_vault / list_pages 集成测试（票据 T-UX 应用壳）：只经引擎公共
//! API 驱动，断言落在真实文件系统（tempdir）。list_pages 的「摄取后」
//! 用例走完整两段式 Ingest（Scripted Fake LLM，离线确定性）。

mod common;

use std::fs;

use common::{fresh_vault_path, tree_snapshot, ScriptedLlmPort};
use corpus_engine::{
    apply, create_vault, ingest, list_pages, open_vault, Error, IngestOutcome, PageKind, TemplateId,
};

// ---------------------------------------------------------------------------
// open_vault
// ---------------------------------------------------------------------------

/// 合法 Vault：返回路径与模板元数据（id + 展示名）。
#[test]
fn open_vault_returns_path_and_template_meta_for_valid_vault() {
    let (_dir, root) = fresh_vault_path("vault");
    create_vault(&root, &TemplateId::from("reading")).expect("create_vault 失败");

    let opened = open_vault(&root).expect("合法 Vault 应能打开");
    assert_eq!(opened.root, root);
    assert_eq!(opened.template_id, TemplateId::from("reading"));
    assert_eq!(opened.template_name, "Reading");

    // 只读校验：打开不改动工作区（也不登记最近列表——那是配置层职责）
    assert!(fs::read_to_string(root.join(".corpus/vault.json")).is_ok());
}

/// 缺 .corpus/vault.json（含路径不存在）：NotAVault，错误信息含路径。
#[test]
fn open_vault_rejects_missing_metadata_with_path_in_error() {
    let (_dir, root) = fresh_vault_path("not-a-vault");
    fs::create_dir_all(&root).unwrap();

    let err = open_vault(&root).unwrap_err();
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
    let message = err.to_string();
    assert!(
        message.contains(root.to_string_lossy().as_ref()),
        "错误信息须含被拒路径：{message}"
    );

    // 路径根本不存在：同样明确拒绝而非 panic
    let (_dir2, ghost) = fresh_vault_path("ghost");
    let err = open_vault(&ghost).unwrap_err();
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
}

/// vault.json 绑定的模板 id 未知（如旧版本创建）：NotAVault 而非 UnknownTemplate
/// ——站在「打开」的口径，这个目录对本引擎不可用，统一按「不是有效
/// Vault」报告，错误信息含路径与具体 id。
#[test]
fn open_vault_rejects_unknown_template_id_as_not_a_vault() {
    let (_dir, root) = fresh_vault_path("vault");
    create_vault(&root, &TemplateId::from("generic")).expect("create_vault 失败");
    fs::write(
        root.join(".corpus/vault.json"),
        r#"{"template_id": "ghost-from-old-version"}"#,
    )
    .unwrap();

    let err = open_vault(&root).unwrap_err();
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
    let message = err.to_string();
    assert!(
        message.contains(root.to_string_lossy().as_ref()),
        "错误信息须含路径：{message}"
    );
    assert!(
        message.contains("ghost-from-old-version"),
        "须指明未知 id：{message}"
    );
}

// ---------------------------------------------------------------------------
// list_pages
// ---------------------------------------------------------------------------

/// 空 Vault（刚创建，wiki/ 只有引擎自维护的 index.md / log.md）：页面树为空
/// ——index.md / log.md 位于 wiki/ 根而非 PageKind 目录，不计入。
#[test]
fn list_pages_on_fresh_vault_returns_empty_tree() {
    let (_dir, root) = fresh_vault_path("vault");
    create_vault(&root, &TemplateId::from("generic")).expect("create_vault 失败");

    let tree = list_pages(&root).expect("list_pages 失败");
    assert!(
        tree.groups.is_empty(),
        "全新 Vault 不应有任何分组：{tree:?}"
    );

    // 非 Vault 目录（缺 wiki/）：明确报错而非假空树
    let (_dir2, plain) = fresh_vault_path("plain");
    fs::create_dir_all(&plain).unwrap();
    assert!(matches!(
        list_pages(&plain).unwrap_err(),
        Error::NotAVault { .. }
    ));
}

/// 完整摄取后：三类分组齐备、路径与顺序正确、updated 有值；手工放的
/// 无 frontmatter 页面宽松列出（updated 为 None），非 .md 与 `._*` 垃圾跳过。
#[tokio::test]
async fn list_pages_groups_ingested_pages_by_kind() {
    let (_dir, root) = fresh_vault_path("vault");
    create_vault(&root, &TemplateId::from("generic")).expect("create_vault 失败");
    fs::write(root.join("raw/paper.md"), "论文正文……").unwrap();

    // 标准两步脚本：1 个 Source + 1 个 Entity + 1 个 Concept（slugs 与
    // tests/ingest.rs 的标准脚手架一致）
    let analysis = r#"{
  "source_title": "Attention Is All You Need",
  "source_summary": "提出 Transformer 架构的论文。",
  "entities": [{"title": "Transformer", "aliases": [], "tags": [], "summary": "架构。", "related": []}],
  "concepts": [{"title": "自注意力", "aliases": [], "tags": [], "summary": "机制。", "related": []}]
}"#;
    let bodies = r##"{
  "pages": [
    {"path": "sources/attention-is-all-you-need", "body": "摘要。"},
    {"path": "entities/transformer", "body": "架构。"},
    {"path": "concepts/自注意力", "body": "机制。"}
  ]
}"##;
    let fast = ScriptedLlmPort::new(vec![analysis.to_string()]);
    let strong = ScriptedLlmPort::new(vec![bodies.to_string()]);
    let outcome = ingest(&root, "raw/paper.md", &fast, &strong, |_| {})
        .await
        .expect("ingest 失败");
    let draft = match outcome {
        IngestOutcome::Draft(draft) => draft,
        IngestOutcome::Skipped(_) => panic!("全新 raw 不应被跳过"),
    };
    apply(&root, &draft, |_| {}).expect("apply 失败");

    // 干扰项：非 .md、macOS 资源叉、无 frontmatter 的手工页面
    fs::write(root.join("wiki/entities/notes.txt"), "not a page").unwrap();
    fs::write(root.join("wiki/entities/._transformer.md"), "junk").unwrap();
    fs::write(
        root.join("wiki/concepts/hand-written.md"),
        "# 手写页\n\n没有 frontmatter。\n",
    )
    .unwrap();

    let tree = list_pages(&root).expect("list_pages 失败");
    let kinds: Vec<PageKind> = tree.groups.iter().map(|g| g.kind).collect();
    assert_eq!(
        kinds,
        [PageKind::Source, PageKind::Entity, PageKind::Concept]
    );

    /// 取指定类型的分组（缺则 panic：前面的 kinds 断言已保证存在）。
    fn group(tree: &corpus_engine::PageTree, kind: PageKind) -> &corpus_engine::PageGroup {
        tree.groups
            .iter()
            .find(|g| g.kind == kind)
            .unwrap_or_else(|| panic!("缺 {kind:?} 分组"))
    }

    let sources = group(&tree, PageKind::Source);
    assert_eq!(sources.pages.len(), 1);
    assert_eq!(
        sources.pages[0].path,
        "sources/attention-is-all-you-need.md"
    );
    assert!(
        sources.pages[0].updated.is_some(),
        "引擎产出的页面 updated 必有值"
    );
    assert_eq!(
        sources.pages[0].title.as_deref(),
        Some("Attention Is All You Need"),
        "T4：页面树条目携带 frontmatter title"
    );

    let entities = group(&tree, PageKind::Entity);
    assert_eq!(
        entities.pages.len(),
        1,
        "notes.txt 与 ._transformer.md 应被跳过"
    );
    assert_eq!(entities.pages[0].path, "entities/transformer.md");
    assert_eq!(entities.pages[0].title.as_deref(), Some("Transformer"));
    assert!(
        entities.pages[0]
            .aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case("Transformer")),
        "aliases 应含 title 自身（merge_aliases 不变量）：{:?}",
        entities.pages[0].aliases
    );

    let concepts = group(&tree, PageKind::Concept);
    let paths: Vec<&str> = concepts.pages.iter().map(|p| p.path.as_str()).collect();
    assert_eq!(paths, ["concepts/hand-written.md", "concepts/自注意力.md"]);
    assert_eq!(
        concepts.pages[0].updated, None,
        "无 frontmatter 的手工页面宽松列出，updated 为 None"
    );
    assert_eq!(
        concepts.pages[0].title, None,
        "无 frontmatter 的手工页面 title 同样宽松为 None"
    );
    assert!(
        concepts.pages[0].aliases.is_empty(),
        "无 frontmatter 的手工页面 aliases 为空"
    );
    assert!(concepts.pages[1].updated.is_some());
    assert_eq!(concepts.pages[1].title.as_deref(), Some("自注意力"));

    // 纯遍历：调用前后全树快照一致（零写入）
    let before = tree_snapshot(&root, "wiki");
    list_pages(&root).expect("第二次 list_pages 失败");
    let after = tree_snapshot(&root, "wiki");
    assert_eq!(before, after, "list_pages 不应产生任何写入");
}
