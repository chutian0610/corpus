//! 原子写：tempfile 与目标同目录 → `persist` 原子 rename（T3 调研笔记
//! 第 3 节）。
//!
//! 关键点：`NamedTempFile::new_in(目标父目录)` 保证同文件系统（跨设备
//! rename 会 EXDEV 失败）；`persist` 对已存在目标是原子替换；commit 前
//! `sync_all` 保证数据落盘。

use std::io::Write;
use std::path::Path;

use crate::error::Result;

/// 原子写入文件：全程要么旧内容、要么新内容，不存在半写。
pub(crate) fn atomic_write(path: &Path, contents: &str) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|err| {
        std::io::Error::new(
            err.error.kind(),
            format!("persist {}: {err}", path.display()),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_existing_content_and_creates_new() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let target = dir.path().join("a.md");

        atomic_write(&target, "第一版").expect("首次写入失败");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第一版");

        atomic_write(&target, "第二版").expect("覆盖写入失败");
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "第二版");
    }

    #[test]
    fn atomic_write_to_missing_directory_fails_without_partial_files() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let target = dir.path().join("no-such-dir/a.md");
        assert!(atomic_write(&target, "x").is_err());
        assert!(!target.exists());
    }
}
