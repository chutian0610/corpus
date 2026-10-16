//! 指纹层：raw 文件内容的 SHA-256 与 Vault 级缓存（`.corpus/state.db`）。
//!
//! 指纹未变则整个 Ingest 管道跳过（零 LLM 调用）。缓存存 sqlite、被
//! `.corpus/.gitignore` 忽略——它可由 raw/ 全量重算，属可再生数据，
//! 不进 git 历史（见 vault 模块的骨架常量）。

use std::path::Path;

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use crate::error::Result;

/// state.db 文件名（位于 Vault 的 `.corpus/` 下）。
pub const STATE_DB_FILENAME: &str = "state.db";

/// 计算字节的 SHA-256（十六进制小写）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Vault 级指纹缓存：raw 相对路径 → SHA-256。
///
/// 同步 sqlite（与 ConfigStore 同一模型：低并发单文件库）；构造即建表
/// （幂等）。`key` 约定为 raw 文件相对 Vault 根的路径。
pub struct StateStore {
    conn: Connection,
}

impl StateStore {
    /// 打开 Vault 的 `.corpus/state.db`（不存在则创建）。
    pub fn open(vault_root: impl AsRef<Path>) -> Result<Self> {
        let dir = vault_root.as_ref().join(".corpus");
        std::fs::create_dir_all(&dir)?;
        Self::open_at(&dir.join(STATE_DB_FILENAME))
    }

    /// 直接按 db 文件路径打开（测试 / 非标准布局用）。
    pub fn open_at(db_path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS fingerprints (
                 key   TEXT PRIMARY KEY,
                 sha256 TEXT NOT NULL
             );",
        )?;
        Ok(Self { conn })
    }

    /// 读取 `key` 的指纹；未摄取过返回 `None`。
    pub fn fingerprint(&self, key: &str) -> Result<Option<String>> {
        let sha = self
            .conn
            .query_row(
                "SELECT sha256 FROM fingerprints WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(sha)
    }

    /// 记录 / 覆盖 `key` 的指纹（raw 内容变化后重摄取时更新）。
    pub fn set_fingerprint(&self, key: &str, sha256: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO fingerprints (key, sha256) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET sha256 = excluded.sha256",
            (key, sha256),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NIST FIPS 180-2 附录 B 的标准测试向量。
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // UTF-8 字节序：中文内容指纹稳定
        assert_eq!(
            sha256_hex("知识库".as_bytes()),
            sha256_hex("知识库".as_bytes()),
        );
    }

    #[test]
    fn state_store_round_trips_and_upserts() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let store = StateStore::open_at(dir.path().join("state.db")).expect("打开失败");

        assert_eq!(store.fingerprint("raw/a.md").unwrap(), None);
        store
            .set_fingerprint("raw/a.md", "sha-1")
            .expect("写入失败");
        assert_eq!(store.fingerprint("raw/a.md").unwrap(), Some("sha-1".into()));
        // 重摄取（内容变化）覆盖
        store
            .set_fingerprint("raw/a.md", "sha-2")
            .expect("覆盖失败");
        assert_eq!(store.fingerprint("raw/a.md").unwrap(), Some("sha-2".into()));
    }

    #[test]
    fn state_store_open_creates_under_corpus_dir() {
        let dir = tempfile::tempdir().expect("tempdir 失败");
        let root = dir.path().join("vault");
        std::fs::create_dir_all(&root).expect("建目录失败");
        let store = StateStore::open(&root).expect("打开失败");
        store.set_fingerprint("raw/x.md", "h").expect("写入失败");
        assert!(root.join(".corpus/state.db").is_file());
        // 重开可读（幂等建表）
        let reopened = StateStore::open(&root).expect("重开失败");
        assert_eq!(reopened.fingerprint("raw/x.md").unwrap(), Some("h".into()));
    }
}
