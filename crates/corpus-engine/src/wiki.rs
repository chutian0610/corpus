//! Wiki 浏览：`list_pages` 纯文件系统遍历的页面树（应用壳 T-UX）。
//!
//! 与 Ingest 管道的 [`crate::ingest::scan_existing_pages`]（全量解析
//! frontmatter 供精确名去重）分工：本模块只回答「wiki/ 下有哪些页面
//! 文件、各自最后更新于何时」，供文件面板与状态栏展示——零 LLM、
//! 零写入；`updated` 只做宽松窥探（只读该字段，页面 schema 不合规
//! 不影响列出，只是该项为 `None`）。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::page::{split_frontmatter, PageKind};

/// [`list_pages`] 的结果：按页面类型分组的页面清单。
#[derive(Debug, Clone, Serialize)]
pub struct PageTree {
    /// 非空分组，按 [`PageKind::ALL`] 固定顺序（sources / entities / concepts）。
    pub groups: Vec<PageGroup>,
}

/// 单个类型分组。
#[derive(Debug, Clone, Serialize)]
pub struct PageGroup {
    /// 页面类型。
    pub kind: PageKind,
    /// 该类型的页面清单（按路径升序）。
    pub pages: Vec<PageEntry>,
}

/// 单个页面条目。
#[derive(Debug, Clone, Serialize)]
pub struct PageEntry {
    /// 相对 `wiki/` 的 `.md` 路径（含类型子目录前缀，如 `entities/transformer.md`）。
    pub path: String,
    /// frontmatter 的 `updated` 时间（RFC 3339）。页面缺 frontmatter /
    /// YAML 非法 / 无该字段时为 `None`（界面不展示），不影响列出。
    pub updated: Option<String>,
}

/// 只窥探 `updated` 一个字段的宽松反序列化目标：serde 忽略未知字段，
/// 字段缺失 / 类型不符 / YAML 整体非法都落到 `None`。
#[derive(Debug, Default, Deserialize)]
struct UpdatedOnly {
    #[serde(default)]
    updated: Option<String>,
}

/// 宽松读取页面 frontmatter 的 `updated`（读不出即 `None`，不报错）。
fn peek_updated(content: &str) -> Option<String> {
    let (yaml, _) = split_frontmatter(content)?;
    serde_yaml_ng::from_str::<UpdatedOnly>(&yaml)
        .ok()?
        .updated
        .filter(|t| !t.trim().is_empty())
}

/// 遍历 `wiki/` 下各 PageKind 目录，返回按类型分组的页面树（公共 API）。
///
/// - 纯文件系统遍历 + frontmatter 宽松窥探：零 LLM、零写入；
/// - 跳过非 `.md` 文件与 macOS 资源叉垃圾（`._*`）；
/// - 类型子目录支持嵌套（引擎当前只写平铺路径，遍历不预设该限制）；
/// - 目录不存在的类型不出现在结果中；`wiki/` 本身缺失说明不是有效
///   Vault，报 [`Error::NotAVault`]。
pub fn list_pages(vault_root: impl AsRef<Path>) -> Result<PageTree> {
    let root = vault_root.as_ref();
    let wiki = root.join("wiki");
    if !wiki.is_dir() {
        return Err(Error::NotAVault {
            path: root.to_path_buf(),
            reason: "缺 wiki/ 目录".to_string(),
        });
    }
    let mut groups = Vec::new();
    for kind in PageKind::ALL {
        let mut pages = Vec::new();
        collect_md_pages(&wiki.join(kind.dir()), kind.dir(), &mut pages)?;
        if !pages.is_empty() {
            pages.sort_by(|a, b| a.path.cmp(&b.path));
            groups.push(PageGroup { kind, pages });
        }
    }
    Ok(PageTree { groups })
}

/// 递归收集 `dir` 下的 `.md` 文件为页面条目（读内容窥探 `updated`）。
///
/// `dir` 不存在视为空（全新 Vault 尚无该类型页面）；目录存在但读取
/// 失败（权限等）则如实上抛——静默吞掉会让文件面板展示假空树。
/// 单个页面文件读取失败只跳过该页（不阻断整棵树）。
fn collect_md_pages(dir: &Path, prefix: &str, out: &mut Vec<PageEntry>) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // macOS 资源叉（`._foo.md`）不是页面：按垃圾跳过
        if name.starts_with("._") {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            collect_md_pages(&path, &format!("{prefix}/{name}"), out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            let rel = format!("{prefix}/{name}");
            let updated = fs::read_to_string(&path)
                .ok()
                .as_deref()
                .and_then(peek_updated);
            out.push(PageEntry { path: rel, updated });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_covers_every_kind_exactly_once() {
        // ALL 覆盖每个已知变体且不重复（新增类型时此清单是唯一改动点）
        for kind in [PageKind::Source, PageKind::Entity, PageKind::Concept] {
            assert_eq!(
                PageKind::ALL.iter().filter(|k| **k == kind).count(),
                1,
                "{kind:?} 应在 ALL 中恰好出现一次"
            );
        }
        assert_eq!(PageKind::ALL.len(), 3);
    }

    #[test]
    fn peek_updated_tolerates_missing_or_invalid_frontmatter() {
        let full = "---\ntype: entity\ntitle: T\naliases: [T]\ntags: []\nrelated: []\nsources: []\ncreated: 2026-10-16T10:00:00+08:00\nupdated: 2026-10-16T11:00:00+08:00\n---\n\n正文\n";
        assert_eq!(
            peek_updated(full).as_deref(),
            Some("2026-10-16T11:00:00+08:00")
        );
        // 缺 frontmatter / 缺字段 / 空值 / YAML 非法：一律 None，不报错
        assert_eq!(peek_updated("普通 Markdown，无 frontmatter"), None);
        assert_eq!(peek_updated("---\ntitle: 只有标题\n---\n\n正文\n"), None);
        assert_eq!(peek_updated("---\nupdated: \"\"\n---\n\n"), None);
        assert_eq!(peek_updated("---\nupdated: [broken\n---\n\n"), None);
    }
}
