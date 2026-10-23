//! resolve_page_path 与 read_page 集成测试（票据 T-UX 应用壳）：read_page
//! 等只读入口的路径安全单一关口与页面读取唯一入口。前置拒绝（绝对路径 /
//! `..` / 非 `.md`）与解析后断言（canonicalize 包含在 Vault 内，堵 symlink
//! 逃逸）都断言在真实文件系统（tempdir）上。

mod common;

use std::fs;
use std::path::PathBuf;

use common::fresh_vault_path;

use corpus_engine::{create_vault, read_page_typed, resolve_page_path, Error, TemplateId};

/// 建一个带页面文件的 Vault：`page_rel` 相对 `wiki/`（自动建父目录）。
fn vault_with_page(tag: &str, page_rel: &str) -> (tempfile::TempDir, PathBuf) {
    let (dir, root) = fresh_vault_path(tag);
    create_vault(&root, &TemplateId::from("generic")).expect("create_vault 失败");
    let page = root.join("wiki").join(page_rel);
    fs::create_dir_all(page.parent().expect("页面路径必有父目录")).unwrap();
    fs::write(&page, "# 页面\n").unwrap();
    (dir, root)
}

/// 正常页面与嵌套子目录页面：返回 canonicalize 后的真实路径。
#[test]
fn resolve_page_path_accepts_regular_and_nested_pages() {
    let (_dir, root) = vault_with_page("vault", "entities/transformer.md");
    let resolved = resolve_page_path(&root, "entities/transformer.md").expect("正常页面应通过");
    assert_eq!(
        resolved,
        root.join("wiki/entities/transformer.md")
            .canonicalize()
            .unwrap(),
        "应返回 canonicalize 后的绝对路径"
    );

    // 嵌套子目录：引擎遍历不预设平铺限制（list_pages 同口径）
    let (_dir2, root2) = vault_with_page("vault", "concepts/ml/attention.md");
    let nested =
        resolve_page_path(&root2, "concepts/ml/attention.md").expect("嵌套子目录页面应通过");
    assert!(nested.is_file());
}

/// 绝对路径、`..` 逃逸、开头 `.`、Windows 盘符 / 根分隔符形态：一律
/// InvalidPagePath，错误信息含被拒路径原样。Windows 形态（`C:foo.md` /
/// `\foo.md`）在 Windows 上由逐组件拒绝（Prefix / RootDir）在解析前
/// 拦下；Unix 上无此组件形态，走后续 canonicalize 失败同样被拒。
#[test]
fn resolve_page_path_rejects_absolute_parent_and_dot_segments() {
    let (_dir, root) = vault_with_page("vault", "entities/transformer.md");
    for bad in [
        "/etc/passwd.md",            // 绝对路径
        "../outside.md",             // .. 逃逸
        "entities/../../outside.md", // 中段 .. 拼出逃逸
        "./entities/transformer.md", // 开头 .：只放行纯 Normal 段
        "C:foo.md",                  // Windows 盘符相对路径（Unix 下为普通文件名）
        "\\foo.md",                  // Windows 根分隔符（Unix 下为普通文件名）
    ] {
        let Err(err) = resolve_page_path(&root, bad) else {
            panic!("{bad} 应被拒绝");
        };
        assert!(
            matches!(err, Error::InvalidPagePath { .. }),
            "{bad} 应报 InvalidPagePath，got: {err:?}"
        );
        assert!(
            err.to_string().contains(bad),
            "错误信息须含被拒路径原样：{err}"
        );
    }
}

/// 非 `.md` 后缀与不存在的页面：拒绝，不给假路径。
#[test]
fn resolve_page_path_rejects_non_md_and_missing_pages() {
    let (_dir, root) = vault_with_page("vault", "entities/transformer.md");
    for bad in ["entities/notes.txt", "entities/transformer", "entities/.md"] {
        assert!(
            matches!(
                resolve_page_path(&root, bad).unwrap_err(),
                Error::InvalidPagePath { .. }
            ),
            "{bad} 应被拒绝"
        );
    }
    // 不存在的 .md 页面：canonicalize 失败按错误处理
    assert!(matches!(
        resolve_page_path(&root, "entities/ghost.md").unwrap_err(),
        Error::InvalidPagePath { .. }
    ));
}

/// symlink 指向 Vault 外：join 后 canonicalize 落到 Vault 外的真实文件，
/// 包含断言拒绝（错误指明「逃出 Vault」）；broken symlink 的 canonicalize
/// 失败同样按错误处理。
#[cfg(unix)]
#[test]
fn resolve_page_path_rejects_symlink_escaping_vault() {
    let (dir, root) = vault_with_page("vault", "entities/transformer.md");
    // Vault 外的目标文件（临时目录下、root 之外）
    let outside = dir.path().join("outside-secret.md");
    fs::write(&outside, "Vault 外的机密").unwrap();
    std::os::unix::fs::symlink(&outside, root.join("wiki/entities/escape.md")).unwrap();

    let err = resolve_page_path(&root, "entities/escape.md").unwrap_err();
    assert!(matches!(err, Error::InvalidPagePath { .. }), "got: {err:?}");
    assert!(err.to_string().contains("逃出"), "须指明逃逸原因：{err}");

    // broken symlink：目标不存在，canonicalize 直接失败
    std::os::unix::fs::symlink(
        dir.path().join("no-such-target.md"),
        root.join("wiki/entities/broken.md"),
    )
    .unwrap();
    assert!(matches!(
        resolve_page_path(&root, "entities/broken.md").unwrap_err(),
        Error::InvalidPagePath { .. }
    ));
}

/// read_page_typed（桌面壳与 corpus-server 共用的页面读取唯一入口）：三段
/// 全在引擎——非 Vault 先拒（open_vault 级校验前置）、`..` 逃逸被路径
/// 安全关口拒绝、正常页面读出结构化 frontmatter + 正文（T4）。
#[test]
fn read_page_typed_reads_content_and_rejects_non_vault_and_escape() {
    let (dir, root) = vault_with_page("vault", "sources/hello.md");

    // helper 写的 hello.md 无 frontmatter：宽松降级为 None + 全文
    let typed = read_page_typed(&root, "sources/hello.md").expect("正常读取失败");
    assert!(typed.frontmatter.is_none());
    assert_eq!(typed.body, "# 页面\n");

    // 带 frontmatter 的页面：结构化字段 + 去头正文（别名随 PageFrontmatter 回读）
    let entities = root.join("wiki/entities");
    fs::create_dir_all(&entities).unwrap();
    fs::write(
        entities.join("transformer.md"),
        "---\ntype: entity\ntitle: Transformer\naliases: [Transformer, transformer 架构]\ntags: []\nrelated: []\nsources: []\ncreated: 2026-10-16T10:00:00+08:00\nupdated: 2026-10-16T11:00:00+08:00\n---\n\n架构说明，见 [[concepts/自注意力]]。\n",
    )
    .unwrap();
    let typed = read_page_typed(&root, "entities/transformer.md").expect("读取失败");
    let fm = typed.frontmatter.expect("合法 frontmatter 应有值");
    assert_eq!(fm.title, "Transformer");
    assert_eq!(
        fm.aliases,
        vec!["Transformer".to_string(), "transformer 架构".to_string()]
    );
    assert_eq!(typed.body, "架构说明，见 [[concepts/自注意力]]。\n");

    // wiki/index.md（引擎自维护目录，无 frontmatter）：可读（T4 目录入口）
    let index = read_page_typed(&root, "index.md").expect("index.md 应可读");
    assert!(index.frontmatter.is_none());
    assert!(index.body.contains("Wiki Index"));

    // `..` 逃逸：路径安全关口拒绝（同一错误类型，HTTP / IPC 上同一文案）
    let err = read_page_typed(&root, "../purpose.md").expect_err("逃逸应被拒绝");
    assert!(matches!(err, Error::InvalidPagePath { .. }), "got: {err:?}");
    assert!(err.to_string().contains("非法页面路径"), "got: {err}");

    // 非 Vault 根：open_vault 级校验先于路径校验拒绝（NotAVault）
    let err = read_page_typed(dir.path(), "sources/hello.md").expect_err("非 Vault 应被拒绝");
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
}
