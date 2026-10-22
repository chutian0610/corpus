//! Raw source 导入：把任意位置的文件复制进 Vault 的 `raw/` 目录。
//!
//! [`import_raw`] 是 `raw/` 的唯一写入口（公共 API）。Ingest 的正确流程
//! 是「从任意位置选文件 → 导入（复制）进 raw/ → 摄取」，用户不需要
//! 先手动把文件拷进 raw/。分流语义：
//! - 源在 Vault `raw/` 内（canonicalize 判定，含子目录）→ 原样返回
//!   （`imported = false`，语义：已在库）；
//! - 源在外部 → 复制进 `raw/` 并保留原文件名：同名且 SHA-256 相同 →
//!   去重返回既有路径（`imported = false`）；同名但内容不同 → 数字
//!   后缀递增（`foo.md` → `foo-2.md` → `foo-3.md`，直到不冲突）。
//!
//! 不变量（见 CONTEXT.md「Raw source」）：导入是复制而非移动，源文件
//! 不动；引擎永不改写 `raw/` 内既有内容，导入复制是唯一入口。
//! 目录批量导入属票据 T8，当前明确报错。

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::error::{Error, Result};
use crate::fingerprint::sha256_hex;

/// [`import_raw`] 的结果：导入后的 raw 路径与「是否本次新导入」。
#[derive(Debug, Clone, Serialize)]
pub struct ImportOutcome {
    /// raw 文件相对 Vault 的路径（如 `raw/foo.md`，可直接作为
    /// [`crate::ingest`] 的 `raw_path` 入参）。
    pub raw_rel: String,
    /// raw 文件在 Vault 内的绝对路径。
    pub raw_abs: PathBuf,
    /// 本次是否新复制了文件进 `raw/`（`false` = 已在库，或同内容
    /// 去重命中既有文件）。
    pub imported: bool,
}

/// 把一个任意位置的文件导入 Vault 的 `raw/` 目录（公共 API）。
///
/// 分流语义见模块文档。错误口径：源是目录报
/// [`Error::ImportSourceIsDirectory`]（目录批量导入属票据 T8）；源不
/// 存在 / 无法读取报 [`Error::ImportSourceUnreadable`]；`vault_root`
/// 不是 Vault 报 [`Error::NotAVault`]。
pub fn import_raw<P: AsRef<Path>, Q: AsRef<Path>>(
    vault_root: P,
    src_path: Q,
) -> Result<ImportOutcome> {
    let vault_root = vault_root.as_ref();
    if !vault_root.join("wiki").is_dir() {
        return Err(Error::NotAVault {
            path: vault_root.to_path_buf(),
            reason: "缺 wiki/ 目录".to_string(),
        });
    }
    let src = src_path.as_ref();
    if src.is_dir() {
        return Err(Error::ImportSourceIsDirectory {
            path: src.to_path_buf(),
        });
    }
    // 相对路径相对 vault_root 解析（与 ingest 的 resolve_raw 同口径）；
    // canonicalize 解析 symlink 与相对段（macOS 的 /var → /private/var），
    // 与 canonicalize 后的 raw/ 做前缀比较才可靠。
    let src_abs = if src.is_absolute() {
        src.to_path_buf()
    } else {
        vault_root.join(src)
    };
    let src_canon = src_abs
        .canonicalize()
        .map_err(|err| Error::ImportSourceUnreadable {
            path: src.to_path_buf(),
            reason: err.to_string(),
        })?;
    // vault_root 刚校验过 wiki/ 存在，canonicalize 不会失败。
    let raw_dir = vault_root.canonicalize()?.join("raw");

    // 已在 raw/ 内：原样返回（已在库），零复制。
    if let Ok(rel) = src_canon.strip_prefix(&raw_dir) {
        let rel = rel.to_string_lossy().replace('\\', "/");
        return Ok(ImportOutcome {
            raw_rel: format!("raw/{rel}"),
            raw_abs: src_canon,
            imported: false,
        });
    }

    // 外部文件：读字节 → 同名去重 / 后缀递增 → 复制进 raw/。
    let bytes = fs::read(&src_canon).map_err(|err| Error::ImportSourceUnreadable {
        path: src.to_path_buf(),
        reason: err.to_string(),
    })?;
    let src_sha = sha256_hex(&bytes);
    let file_name = src_canon
        .file_name()
        .expect("canonicalize 后的文件路径必有文件名")
        .to_string_lossy()
        .into_owned();
    fs::create_dir_all(&raw_dir)?; // 骨架保证存在；防御旧 Vault 手删 raw/ 的情况

    let stem = Path::new(&file_name)
        .file_stem()
        .expect("非空文件名必有主干")
        .to_string_lossy()
        .into_owned();
    let ext = Path::new(&file_name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();

    // 候选名序列：原名 → foo-2.md → foo-3.md …（首个不冲突者胜出；
    // 存在但同内容者视为去重命中，返回既有文件）。
    let mut counter = 1usize;
    loop {
        let candidate = if counter == 1 {
            file_name.clone()
        } else {
            format!("{stem}-{counter}{ext}")
        };
        let target = raw_dir.join(&candidate);
        if target.is_file() {
            // 同名已存在：同内容 → 去重返回既有；不同 → 后缀递增再试。
            if sha256_hex(&fs::read(&target)?) == src_sha {
                return Ok(ImportOutcome {
                    raw_rel: format!("raw/{candidate}"),
                    raw_abs: target,
                    imported: false,
                });
            }
            counter += 1;
            continue;
        }
        // fs::copy 字节级复制（含部分元数据）；源文件不动，既有 raw
        // 文件永不触碰（唯一入口只新增文件）。
        fs::copy(&src_canon, &target)?;
        return Ok(ImportOutcome {
            raw_rel: format!("raw/{candidate}"),
            raw_abs: target,
            imported: true,
        });
    }
}
