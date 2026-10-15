//! Vault 注册表：最近打开的 Vault 列表，存于 config.db 的 `recent_vaults` 表。
//!
//! 与 [`crate::config::ConfigStore`] 同库同连接；表结构迁移在
//! [`ConfigStore::open`] 中一并完成。

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::config::ConfigStore;
use crate::error::{Error, Result};
use crate::template::{self, TemplateId};

/// 一条最近打开记录：Vault 路径、绑定模板与最后打开时间。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentVault {
    /// Vault 根目录（主键）。
    pub path: PathBuf,
    /// Vault 绑定的模板 id。
    pub template_id: TemplateId,
    /// 最近一次打开时间。
    pub opened_at: DateTime<Utc>,
}

/// 分文件说明：注册表与配置**同库不同域**——配置回答「模型与行为怎么设」，
/// 注册表回答「哪些 Vault 最近打开过」，分文件把两者的变化原因隔离开。
impl ConfigStore {
    /// 列出最近打开的 Vault，按打开时间降序（最新在前）。
    pub fn list_recent_vaults(&self) -> Result<Vec<RecentVault>> {
        let conn = self.lock_conn();
        let mut stmt = conn.prepare(
            "SELECT path, template_id, opened_at FROM recent_vaults ORDER BY opened_at DESC",
        )?;
        let raw: Vec<(String, String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        drop(conn);
        raw.into_iter()
            .map(|(path, template_id, opened_at)| {
                Ok(RecentVault {
                    path: PathBuf::from(path),
                    template_id: TemplateId::from(template_id),
                    opened_at: DateTime::parse_from_rfc3339(&opened_at)
                        .map_err(|e| {
                            Error::InvalidConfig(format!("注册表 opened_at 解析失败：{e}"))
                        })?
                        .with_timezone(&Utc),
                })
            })
            .collect()
    }

    /// 记录一次「打开 Vault」（创建即打开）：已存在则刷新时间与模板。
    ///
    /// `template_id` 非内置模板时报 [`Error::UnknownTemplate`]，不写入。
    pub fn add_recent_vault(&self, path: impl AsRef<Path>, template_id: &TemplateId) -> Result<()> {
        if template::template_by_id(template_id).is_none() {
            return Err(Error::UnknownTemplate {
                id: template_id.clone(),
                available: template::builtin_templates()
                    .iter()
                    .map(|t| t.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO recent_vaults (path, template_id, opened_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(path) DO UPDATE SET
                 template_id = excluded.template_id,
                 opened_at = excluded.opened_at",
            params![
                path.as_ref().to_string_lossy(),
                template_id.as_str(),
                Utc::now().to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// 从注册表移除一个 Vault；返回是否确有删除（路径不存在则 `false`）。
    pub fn remove_recent_vault(&self, path: impl AsRef<Path>) -> Result<bool> {
        let conn = self.lock_conn();
        let removed = conn.execute(
            "DELETE FROM recent_vaults WHERE path = ?1",
            params![path.as_ref().to_string_lossy()],
        )?;
        Ok(removed > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{GENERIC, READING, RESEARCH};

    fn sorted_paths(vaults: &[RecentVault]) -> Vec<String> {
        let mut paths: Vec<String> = vaults
            .iter()
            .map(|v| v.path.to_string_lossy().into())
            .collect();
        paths.sort();
        paths
    }

    #[test]
    fn empty_registry_lists_nothing() {
        let (_dir, store) = ConfigStore::test_open();
        assert!(store.list_recent_vaults().expect("列注册表失败").is_empty());
    }

    #[test]
    fn add_then_list_and_remove() {
        let (_dir, store) = ConfigStore::test_open();
        store
            .add_recent_vault("/tmp/vault-a", &GENERIC.id)
            .expect("添加失败");
        store
            .add_recent_vault("/tmp/vault-b", &RESEARCH.id)
            .expect("添加失败");

        let list = store.list_recent_vaults().expect("列注册表失败");
        assert_eq!(
            sorted_paths(&list),
            vec!["/tmp/vault-a".to_string(), "/tmp/vault-b".to_string()]
        );
        assert!(list.iter().all(|v| v.opened_at <= Utc::now()
            && v.opened_at > Utc::now() - chrono::Duration::minutes(1)));

        assert!(store.remove_recent_vault("/tmp/vault-a").expect("移除失败"));
        let list = store.list_recent_vaults().expect("列注册表失败");
        assert_eq!(sorted_paths(&list), vec!["/tmp/vault-b".to_string()]);
        // 再删同一项：无行受影响
        assert!(!store
            .remove_recent_vault("/tmp/vault-a")
            .expect("重复移除失败"));
    }

    #[test]
    fn readd_same_path_upserts_template_and_time() {
        let (_dir, store) = ConfigStore::test_open();
        let before = Utc::now();
        store
            .add_recent_vault("/tmp/vault-a", &GENERIC.id)
            .expect("添加失败");
        std::thread::sleep(std::time::Duration::from_millis(1100)); // RFC3339 秒级精度
        store
            .add_recent_vault("/tmp/vault-a", &READING.id)
            .expect("覆盖添加失败");

        let list = store.list_recent_vaults().expect("列注册表失败");
        assert_eq!(list.len(), 1, "同一路径必须 upsert 而非新增");
        assert_eq!(list[0].template_id, READING.id);
        // upsert 必须刷新 opened_at（第二次添加晚于第一次 before + 1s）
        assert!(list[0].opened_at > before + chrono::Duration::seconds(1));
    }

    #[test]
    fn add_unknown_template_is_rejected() {
        let (_dir, store) = ConfigStore::test_open();
        let err = store
            .add_recent_vault("/tmp/vault-x", &TemplateId::from("no-such-template"))
            .expect_err("未知模板应报错");
        assert!(matches!(err, Error::UnknownTemplate { .. }));
        assert!(store.list_recent_vaults().expect("列注册表失败").is_empty());
    }

    /// 注册表真实落库：绕过公共 API 直读表行。
    #[test]
    fn registry_persists_real_rows_in_sqlite() {
        let (dir, store) = ConfigStore::test_open();
        store
            .add_recent_vault("/tmp/vault-db", &GENERIC.id)
            .expect("添加失败");

        let conn = rusqlite::Connection::open(dir.path().join(crate::config::CONFIG_DB_FILENAME))
            .expect("直连 db 失败");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM recent_vaults", [], |row| row.get(0))
            .expect("统计失败");
        assert_eq!(count, 1);
        let (template_id,): (String,) = conn
            .query_row("SELECT template_id FROM recent_vaults", [], |row| {
                Ok((row.get(0)?,))
            })
            .expect("读取行失败");
        assert_eq!(template_id, "generic");
    }
}
