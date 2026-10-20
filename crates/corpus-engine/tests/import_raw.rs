//! import_raw 集成测试：只经引擎公共 API 驱动，断言落在真实文件系统
//!（字节级一致、文件名保留、去重不产生第二份、后缀递增各保留、源
//! 文件不动、raw/ 内直传零复制）。目录批量导入属票据 T8，当前报错。

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{fresh_vault_path, tree_snapshot};
use corpus_engine::{create_vault, import_raw, Error, TemplateId};

/// 造一个绑定 Generic 模板的全新 Vault。
fn setup_vault(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = fresh_vault_path(tag);
    create_vault(&root, &TemplateId::from("generic")).expect("create_vault 失败");
    (dir, root)
}

/// 在 Vault 外（独立临时目录）造一个源文件，返回其路径。
/// TempDir 随句柄存活，测试期间源文件不会被清理。
fn write_external(name: &str, content: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let path = dir.path().join(name);
    fs::write(&path, content).expect("写外部源文件失败");
    (dir, path)
}

/// raw/ 下的 .md 文件名清单（排序后；.gitkeep 不算内容文件）。
fn list_raw_md(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join("raw"))
        .expect("列 raw/ 失败")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".md"))
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// 外部文件导入
// ---------------------------------------------------------------------------

/// 验收：外部文件导入后字节级一致、保留原文件名（含中文名）、
/// imported=true，raw_rel / raw_abs 指向新文件。
#[test]
fn external_file_is_copied_byte_identical_with_original_name() {
    let (_vault_dir, root) = setup_vault("vault");
    let content = "外部内容，含中文与换行。\nsecond line\n";
    let (_src_dir, src) = write_external("论文.md", content);

    let outcome = import_raw(&root, &src).expect("外部导入应成功");

    assert!(outcome.imported, "外部文件导入应为新导入");
    assert_eq!(outcome.raw_rel, "raw/论文.md", "保留原文件名");
    assert_eq!(
        outcome.raw_abs,
        root.canonicalize().unwrap().join("raw/论文.md"),
        "raw_abs 指向 Vault 内的导入文件"
    );
    let copied = fs::read(&outcome.raw_abs).expect("导入文件应存在");
    assert_eq!(copied, content.as_bytes(), "字节级一致");
}

// ---------------------------------------------------------------------------
// 同内容去重
// ---------------------------------------------------------------------------

/// 验收：外部同内容重导 → 去重：imported=false、返回既有路径，
/// raw/ 不出现第二份。
#[test]
fn reimport_same_content_dedupes_to_existing_file() {
    let (_vault_dir, root) = setup_vault("vault");
    let (_src_dir, src) = write_external("a.md", "相同内容");
    let first = import_raw(&root, &src).expect("首次导入应成功");
    assert!(first.imported);

    // 同内容、可来自不同路径的重导 → 去重返回既有文件
    let (_src2_dir, src2) = write_external("a.md", "相同内容");
    let second = import_raw(&root, &src2).expect("同内容重导应成功");
    assert!(!second.imported, "同内容去重不得视为新导入");
    assert_eq!(second.raw_rel, first.raw_rel);
    assert_eq!(second.raw_abs, first.raw_abs);

    // 同名同内容第三份也一样
    let (_src3_dir, src3) = write_external("a.md", "相同内容");
    let third = import_raw(&root, &src3).expect("第三次同内容导入应成功");
    assert!(!third.imported);

    assert_eq!(
        list_raw_md(&root),
        vec!["a.md".to_string()],
        "raw/ 不得出现第二份"
    );
}

// ---------------------------------------------------------------------------
// 同名不同内容：后缀递增
// ---------------------------------------------------------------------------

/// 验收：外部同名不同内容 → foo.md → foo-2.md → foo-3.md 递增，
/// 各文件内容各自保留、互不覆盖。
#[test]
fn same_name_different_content_appends_numeric_suffix() {
    let (_vault_dir, root) = setup_vault("vault");
    let (_d1, s1) = write_external("foo.md", "第一份内容");
    let (_d2, s2) = write_external("foo.md", "第二份内容（不同）");
    let (_d3, s3) = write_external("foo.md", "第三份内容（也不同）");

    let first = import_raw(&root, &s1).expect("第一份导入应成功");
    let second = import_raw(&root, &s2).expect("第二份导入应成功");
    let third = import_raw(&root, &s3).expect("第三份导入应成功");

    assert_eq!(first.raw_rel, "raw/foo.md");
    assert_eq!(second.raw_rel, "raw/foo-2.md", "同名不同内容追加 -2 后缀");
    assert_eq!(third.raw_rel, "raw/foo-3.md", "后续递增到 -3");
    assert!(second.imported && third.imported);

    // 各保留各的内容，互不覆盖
    assert_eq!(
        fs::read_to_string(root.join("raw/foo.md")).unwrap(),
        "第一份内容"
    );
    assert_eq!(
        fs::read_to_string(root.join("raw/foo-2.md")).unwrap(),
        "第二份内容（不同）"
    );
    assert_eq!(
        fs::read_to_string(root.join("raw/foo-3.md")).unwrap(),
        "第三份内容（也不同）"
    );

    // 后缀递增后再导同内容于递增名上 → 命中去重（foo-2 同内容重导不产生 foo-4）
    let (_d4, s4) = write_external("foo-2.md", "第二份内容（不同）");
    let dedup = import_raw(&root, &s4).expect("foo-2 同内容重导应成功");
    assert!(!dedup.imported, "命中既有 foo-2.md 应去重");
    assert_eq!(dedup.raw_rel, "raw/foo-2.md");
    assert_eq!(
        list_raw_md(&root),
        vec![
            "foo-2.md".to_string(),
            "foo-3.md".to_string(),
            "foo.md".to_string()
        ]
    );
}

// ---------------------------------------------------------------------------
// raw/ 内直传
// ---------------------------------------------------------------------------

/// 验收：src 已在 Vault raw/ 内（绝对或相对路径均可）→ 原样返回、
/// imported=false、raw/ 逐字节不变（零复制）。
#[test]
fn path_already_in_raw_passes_through_without_copy() {
    let (_vault_dir, root) = setup_vault("vault");
    fs::write(root.join("raw/a.md"), "库内内容").expect("写 raw 失败");
    let before = tree_snapshot(&root, "raw");

    // 绝对路径直传
    let abs = import_raw(&root, root.join("raw/a.md")).expect("raw 内直传应成功");
    assert!(!abs.imported, "已在库语义 imported=false");
    assert_eq!(abs.raw_rel, "raw/a.md");
    assert_eq!(abs.raw_abs, root.canonicalize().unwrap().join("raw/a.md"));

    // 相对路径（相对 vault_root）直传
    let rel = import_raw(&root, "raw/a.md").expect("相对路径直传应成功");
    assert!(!rel.imported);
    assert_eq!(rel.raw_abs, abs.raw_abs);

    assert_eq!(tree_snapshot(&root, "raw"), before, "raw/ 不得有任何改动");
}

// ---------------------------------------------------------------------------
// 源文件不被移动 / 修改
// ---------------------------------------------------------------------------

/// 验收：导入是复制不是移动——源文件留在原地且字节不变。
#[test]
fn source_file_is_never_moved_or_modified() {
    let (_vault_dir, root) = setup_vault("vault");
    let (_src_dir, src) = write_external("keep.md", "源内容，一字不改");
    import_raw(&root, &src).expect("导入应成功");

    assert!(src.is_file(), "源文件必须留在原地（复制而非移动）");
    assert_eq!(
        fs::read_to_string(&src).unwrap(),
        "源内容，一字不改",
        "源文件内容不得被修改"
    );
    // Vault 内有副本
    assert_eq!(
        fs::read_to_string(root.join("raw/keep.md")).unwrap(),
        "源内容，一字不改"
    );
}

// ---------------------------------------------------------------------------
// 目录导入报错（T8）
// ---------------------------------------------------------------------------

/// 验收：src 是目录 → 明确报错且错误信息注明 T8，raw/ 无新文件。
#[test]
fn directory_source_is_rejected_with_t8_note() {
    let (_vault_dir, root) = setup_vault("vault");
    let src_dir = tempfile::tempdir().expect("创建临时目录失败");
    fs::write(src_dir.path().join("inner.md"), "内容").unwrap();

    let err = import_raw(&root, src_dir.path()).expect_err("目录导入必须报错");
    assert!(
        matches!(err, Error::ImportSourceIsDirectory { .. }),
        "got: {err:?}"
    );
    assert!(
        err.to_string().contains("T8"),
        "错误信息须注明目录批量导入属 T8：{err}"
    );
    assert!(list_raw_md(&root).is_empty(), "报错不得留下任何文件");
}

// ---------------------------------------------------------------------------
// 非 Vault 拒绝
// ---------------------------------------------------------------------------

#[test]
fn non_vault_directory_is_rejected() {
    let (dir, root) = fresh_vault_path("not-a-vault");
    fs::create_dir_all(&root).unwrap();
    let (_src_dir, src) = write_external("a.md", "内容");
    let err = import_raw(&root, &src).expect_err("非 Vault 目录必须拒绝");
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
    let _ = dir;
}
