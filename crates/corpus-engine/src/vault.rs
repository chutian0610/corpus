//! Vault：一个目录即一个 git 仓库（Raw sources + Wiki + Purpose），
//! 创建时绑定一个 Template。

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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

/// `.corpus/vault.json`：Vault 与模板的绑定元数据（Ingest 等操作据此
/// 取回创建时选定的模板，见 CONTEXT.md「Vault」）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultMeta {
    /// 创建时绑定的模板 id。
    pub template_id: TemplateId,
}

/// `.corpus/.gitignore` 内容：指纹缓存 `state.db` 可由 raw/ 全量重算，
/// 属可再生数据，不入库（git 历史只承载 Wiki 与骨架）。
const CORPUS_GITIGNORE: &str = "state.db\n";

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
    ".corpus/.gitignore",
    ".corpus/vault.json",
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
    // 指纹缓存不入库；模板绑定元数据随仓库走（克隆后模板仍可知）。
    fs::write(root.join(".corpus/.gitignore"), CORPUS_GITIGNORE)?;
    fs::write(
        root.join(".corpus/vault.json"),
        serde_json::to_string_pretty(&VaultMeta {
            template_id: template.id.clone(),
        })?,
    )?;

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

/// 读取 Vault 绑定的模板 id（`.corpus/vault.json`）。
///
/// Ingest 等需要模板的操作以此取回创建时的选择，调用方再经
/// [`crate::template::template_by_id`] 校验并取得模板常量。
/// 路径缺骨架 / 元数据缺失 / 内容非法时报 [`Error::NotAVault`]。
pub fn vault_template_id(root: impl AsRef<Path>) -> Result<TemplateId> {
    let root = root.as_ref();
    let meta_path = root.join(".corpus/vault.json");
    let raw = fs::read_to_string(&meta_path).map_err(|err| Error::NotAVault {
        path: root.to_path_buf(),
        reason: format!("无法读取 {}（{err}）", meta_path.display()),
    })?;
    let meta: VaultMeta = serde_json::from_str(&raw).map_err(|err| Error::NotAVault {
        path: root.to_path_buf(),
        reason: format!("{} 内容非法：{err}", meta_path.display()),
    })?;
    Ok(meta.template_id)
}

/// 路径是「空目录」吗？读取失败按非空处理（保守：宁可拒绝也不覆盖）。
fn is_empty_dir(path: &Path) -> bool {
    path.is_dir()
        && fs::read_dir(path)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(false)
}
