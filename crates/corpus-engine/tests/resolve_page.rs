//! resolve_page_path 集成测试（票据 T-UX 应用壳）：read_page 等只读入口
//! 的路径安全单一关口。前置拒绝（绝对路径 / `..` / 非 `.md`）与解析后
//! 断言（canonicalize 包含在 Vault 内，堵 symlink 逃逸）都断言在真实
//! 文件系统（tempdir）上。

mod common;

use std::fs;
use std::path::PathBuf;

use common::fresh_vault_path;
use corpus_engine::{create_vault, resolve_page_path, Error, TemplateId};

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
