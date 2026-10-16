//! Vault：一个目录即一个 git 仓库（Raw sources + Wiki + Purpose），
//! 创建时绑定一个 Template。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::git;
use crate::template::{self, TemplateId};

/// `create_vault` 的结果：Vault 位置与生成的骨架清单。
#[derive(Debug, Clone, Serialize)]
pub struct VaultInfo {
    /// Vault 根目录（调用方传入的路径）。
    pub root: PathBuf,
    /// 所选模板 id。
    pub template_id: TemplateId,
    /// 所选模板展示名。
    pub template_name: String,
    /// 生成的骨架文件与目录（相对 `root`）。
    pub created_paths: Vec<String>,
}

/// Vault 初始提交信息。
pub const INITIAL_COMMIT_MESSAGE: &str = "corpus: init vault";

/// `wiki/index.md` 初始内容（内容目录，由引擎维护）。
const INDEX_MD: &str = "\
# Wiki Index

本文件由 Corpus 引擎维护，是 Wiki 的内容目录。请勿手工编辑。

## 页面

（暂无页面）
";

/// `wiki/log.md` 的文件头；其后跟第一条可 grep 的时序记录。
const LOG_MD_HEADER: &str = "\
# Wiki Log

时序操作日志，由 Corpus 引擎维护。每条记录一行，格式：
`## [YYYY-MM-DD] <操作> | <标题>`
";

/// 骨架清单（相对 root，用于 `VaultInfo::created_paths` 与界面展示）。
const SKELETON: &[&str] = &[
    "raw/",
    "raw/.gitkeep",
    "wiki/",
    "wiki/index.md",
    "wiki/log.md",
    "purpose.md",
    ".corpus/",
    ".corpus/.gitkeep",
];

/// 在 `path` 创建一个绑定 `template_id` 的新 Vault（公共 API）。
///
/// 生成目录骨架：`raw/`、`wiki/index.md`、`wiki/log.md`、`purpose.md`、
/// `.corpus/`，随后 `git init` 并完成初始提交。
///
/// - `path` 已存在且非空（文件或含内容的目录）时报 [`Error::VaultPathExists`]，不覆盖；
/// - `path` 已存在但为空目录时允许（目录选择对话框只能选中已存在的目录）；
/// - `template_id` 非内置模板时报 [`Error::UnknownTemplate`]，不创建任何文件；
/// - T1 阶段本操作不调用 LLM。
pub fn create_vault<P: AsRef<Path>>(path: P, template_id: &TemplateId) -> Result<VaultInfo> {
    let root = path.as_ref();
    if root.exists() && !is_empty_dir(root) {
        return Err(Error::VaultPathExists {
            path: root.to_path_buf(),
        });
    }
    let template = template::template_by_id(template_id).ok_or_else(|| Error::UnknownTemplate {
        id: template_id.clone(),
        available: template::builtin_templates()
            .iter()
            .map(|t| t.id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    })?;

    // 目录骨架
    fs::create_dir_all(root.join("raw"))?;
    fs::create_dir_all(root.join("wiki"))?;
    fs::create_dir_all(root.join(".corpus"))?;
    // git 不跟踪空目录：raw/ 与 .corpus/ 放 .gitkeep 占位，
    // 保证初始提交后克隆 / 回滚仍保留目录结构。
    fs::write(root.join("raw/.gitkeep"), "")?;
    fs::write(root.join(".corpus/.gitkeep"), "")?;

    // 内容目录
    fs::write(root.join("wiki/index.md"), INDEX_MD)?;
    // 时序日志：首条记录为可 grep 的 `## [YYYY-MM-DD] init | Vault`
    let today = chrono::Local::now().format("%Y-%m-%d");
    fs::write(
        root.join("wiki/log.md"),
        format!("{LOG_MD_HEADER}\n## [{today}] init | Vault\n"),
    )?;
    // Purpose：内容来自所选模板
    fs::write(root.join("purpose.md"), template.purpose_template)?;

    // git init + 初始提交
    git::init_with_initial_commit(root, INITIAL_COMMIT_MESSAGE)?;

    Ok(VaultInfo {
        root: root.to_path_buf(),
        template_id: template.id.clone(),
        template_name: template.name.to_string(),
        created_paths: SKELETON.iter().map(|p| (*p).to_string()).collect(),
    })
}

/// 路径是「空目录」吗？读取失败按非空处理（保守：宁可拒绝也不覆盖）。
fn is_empty_dir(path: &Path) -> bool {
    path.is_dir()
        && fs::read_dir(path)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
}
