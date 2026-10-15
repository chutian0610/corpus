//! 全局配置层：应用数据目录下 `config.db`（sqlite）中的模型与行为配置。
//!
//! 设计约束（见 T2 调研笔记第 5、8 节）：
//! - rusqlite（bundled）同步 API + `Mutex<Connection>` 包裹——SQLite 单文件
//!   低并发的本质与同步模型匹配，壳层（Tauri command）已在线程池执行；
//! - **API key 只存环境变量名**（[`ModelTier::api_key_env`]），值永不落盘；
//! - 引擎只接受目录参数（[`ConfigStore::open`]），`tempfile::tempdir()` 即可测试。
//!
//! Vault 注册表（最近打开）与配置同库，见 [`crate::registry`]。

use std::fs;
use std::path::Path;
use std::sync::Mutex;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// config.db 文件名（位于传入目录下）。
pub const CONFIG_DB_FILENAME: &str = "config.db";

/// 配置在 `config` 表中的 key（值为整个 [`AppConfig`] 的 JSON）。
const CONFIG_KEY: &str = "app_config";

/// LLM 供应方类型：决定 provider 工厂如何构造客户端（T2 调研笔记第 2 节）。
///
/// serde 为小写蛇形字符串（如 `openai_compatible`），显式 rename 与
/// [`ProviderKind::as_str`] 保持一致；更换供应商只改配置，引擎代码零改动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderKind {
    /// OpenAI 官方端点。
    #[serde(rename = "openai")]
    OpenAI,
    /// Anthropic 官方端点（Messages API）。
    #[serde(rename = "anthropic")]
    Anthropic,
    /// DeepSeek 官方端点（OpenAI 方言客户端）。
    #[serde(rename = "deepseek")]
    DeepSeek,
    /// 任意 OpenAI 兼容端点：必须配置 base_url，工厂启用
    /// `Route::Chat` + system-instructions-as-messages 兜底。
    #[serde(rename = "openai_compatible")]
    OpenAICompatible,
}

impl ProviderKind {
    /// 全部供应方（展示顺序）。
    pub const ALL: &'static [ProviderKind] = &[
        ProviderKind::OpenAI,
        ProviderKind::Anthropic,
        ProviderKind::DeepSeek,
        ProviderKind::OpenAICompatible,
    ];

    /// 该供应方惯例的 API key 环境变量名；`openai_compatible` 无惯例
    /// （自定义端点配自定义变量名），返回 `None`。
    pub fn default_api_key_env(self) -> Option<&'static str> {
        match self {
            ProviderKind::OpenAI => Some("OPENAI_API_KEY"),
            ProviderKind::Anthropic => Some("ANTHROPIC_API_KEY"),
            ProviderKind::DeepSeek => Some("DEEPSEEK_API_KEY"),
            ProviderKind::OpenAICompatible => None,
        }
    }

    /// id 字符串（与 serde 表示一致，展示与日志用）。
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::OpenAI => "openai",
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::DeepSeek => "deepseek",
            ProviderKind::OpenAICompatible => "openai_compatible",
        }
    }
}

/// 一个模型档位：provider、模型名与接入参数。
///
/// `api_key_env` 是**环境变量名**而非 key 值；`None` 时按
/// [`ProviderKind::default_api_key_env`] 的惯例名解析。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTier {
    /// 供应方类型。
    pub provider: ProviderKind,
    /// 模型名（如 `gpt-5.2`、`claude-sonnet-4-6`）；空串表示尚未配置。
    pub model: String,
    /// 自定义 base_url（OpenAI / Anthropic / OpenAI 兼容端点可选或必填）。
    pub base_url: Option<String>,
    /// API key 所在的环境变量名；`None` 落回供应方惯例名。
    pub api_key_env: Option<String>,
}

impl ModelTier {
    /// 解析实际使用的 API key 环境变量名（显式配置优先，落回惯例名）。
    pub fn api_key_env_name(&self) -> Option<&str> {
        self.api_key_env
            .as_deref()
            .or_else(|| self.provider.default_api_key_env())
    }

    /// 从环境变量读取 API key 值。变量未设置时报 [`Error::MissingApiKey`]。
    ///
    /// key 值只在本方法调用瞬间进入内存，任何持久化路径都不经过它。
    pub fn resolve_api_key(&self) -> Result<String> {
        let env = self.api_key_env_name().ok_or_else(|| {
            Error::InvalidConfig(format!(
                "未配置 API key 环境变量名（provider：{}，请显式指定 api_key_env）",
                self.provider.as_str()
            ))
        })?;
        std::env::var(env).map_err(|_| Error::MissingApiKey {
            env: env.to_string(),
        })
    }
}

/// Apply 模式：Draft 落盘是自动执行还是人审触发（见 CONTEXT.md「Apply」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyMode {
    /// 自动执行。
    Auto,
    /// 人审触发。
    Manual,
}

/// 全局应用配置：双模型档位 + Apply 模式 + Query 沉淀开关。
///
/// 经 [`ConfigStore::get_config`] / [`ConfigStore::set_config`] 读写，
/// 整体序列化为 JSON 存入 config 表。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    /// 快速档：高频、低延迟任务（页面路由、摘要等）。
    pub fast: ModelTier,
    /// 强力档：复杂推理任务（成文、矛盾检测等）。
    pub strong: ModelTier,
    /// Apply 模式，默认人审（安全优先）。
    pub apply_mode: ApplyMode,
    /// Query page 沉淀开关（默认开，见 CONTEXT.md「Query page」）。
    pub query_pages: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            fast: ModelTier::default_for(ProviderKind::OpenAI),
            strong: ModelTier::default_for(ProviderKind::OpenAI),
            apply_mode: ApplyMode::Manual,
            query_pages: true,
        }
    }
}

impl ModelTier {
    /// 指定 provider 的初始档位：模型名留空（视为尚未配置，连通测试会
    /// 给出明确错误），api_key_env 落回惯例名（`None`）。
    fn default_for(provider: ProviderKind) -> Self {
        Self {
            provider,
            model: String::new(),
            base_url: None,
            api_key_env: None,
        }
    }
}

/// 全局配置与 Vault 注册表的 sqlite 存储。
///
/// 线程安全：`Mutex<Connection>` 包裹同步连接，Tauri command /
/// 测试并发调用无需额外协调。构造即迁移（幂等建表）。
pub struct ConfigStore {
    pub(crate) conn: Mutex<Connection>,
}

impl ConfigStore {
    /// 打开 `{dir}/config.db`（不存在则创建）并执行迁移。
    ///
    /// `dir` 不存在时会先创建；壳层传入 `app_data_dir()`，测试传 tempdir。
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)?;
        let conn = Connection::open(dir.join(CONFIG_DB_FILENAME))?;
        Self::migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// 建表迁移（幂等）：`config`（KV 配置）与 `recent_vaults`（注册表）。
    fn migrate(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS config (
                 key   TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS recent_vaults (
                 path        TEXT PRIMARY KEY,
                 template_id TEXT NOT NULL,
                 opened_at   TEXT NOT NULL
             );",
        )?;
        Ok(())
    }

    /// 读取应用配置；库中无记录时返回 [`AppConfig::default`]。
    pub fn get_config(&self) -> Result<AppConfig> {
        let conn = self.conn.lock().expect("config db 连接锁中毒");
        let json: Option<String> = conn
            .query_row(
                "SELECT value FROM config WHERE key = ?1",
                [CONFIG_KEY],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        match json {
            Some(json) => Ok(serde_json::from_str(&json)?),
            None => Ok(AppConfig::default()),
        }
    }

    /// 保存应用配置（整体覆盖写）。
    pub fn set_config(&self, config: &AppConfig) -> Result<()> {
        let json = serde_json::to_string(config)?;
        let conn = self.conn.lock().expect("config db 连接锁中毒");
        conn.execute(
            "INSERT INTO config (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (CONFIG_KEY, json),
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn store() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().expect("创建 tempdir 失败");
        let store = ConfigStore::open(dir.path()).expect("打开 ConfigStore 失败");
        (dir, store)
    }

    #[test]
    fn default_config_when_db_empty() {
        let (_dir, store) = store();
        let config = store.get_config().expect("读取默认配置失败");
        assert_eq!(config, AppConfig::default());
        assert_eq!(config.apply_mode, ApplyMode::Manual);
        assert!(config.query_pages);
        assert_eq!(config.fast.provider, ProviderKind::OpenAI);
        assert_eq!(config.fast.model, "");
        assert_eq!(config.fast.api_key_env_name(), Some("OPENAI_API_KEY"));
    }

    #[test]
    fn set_then_get_roundtrip_via_public_api() {
        let (_dir, store) = store();
        let config = AppConfig {
            fast: ModelTier {
                provider: ProviderKind::OpenAICompatible,
                model: "qwen3-max".into(),
                base_url: Some("https://gw.example.com/v1".into()),
                api_key_env: Some("GW_API_KEY".into()),
            },
            strong: ModelTier {
                provider: ProviderKind::Anthropic,
                model: "claude-sonnet-4-6".into(),
                base_url: None,
                api_key_env: None,
            },
            apply_mode: ApplyMode::Auto,
            query_pages: false,
        };

        store.set_config(&config).expect("保存配置失败");
        assert_eq!(store.get_config().expect("读取配置失败"), config);
    }

    /// 配置必须真实落进 sqlite：绕过公共 API 直接读表内容断言。
    #[test]
    fn set_config_persists_real_row_in_sqlite() {
        let (dir, store) = store();
        let config = AppConfig {
            fast: ModelTier {
                provider: ProviderKind::DeepSeek,
                model: "deepseek-v4-flash".into(),
                base_url: None,
                api_key_env: None,
            },
            ..AppConfig::default()
        };
        store.set_config(&config).expect("保存配置失败");

        let conn = Connection::open(dir.path().join(CONFIG_DB_FILENAME)).expect("直连 db 失败");
        let json: String = conn
            .query_row(
                "SELECT value FROM config WHERE key = 'app_config'",
                [],
                |row| row.get(0),
            )
            .expect("config 表无 app_config 行");
        let raw: serde_json::Value = serde_json::from_str(&json).expect("存储值非 JSON");
        // 直接断言 sqlite 行内的 JSON 真实内容，而非只走公共 API
        assert_eq!(raw["fast"]["model"], "deepseek-v4-flash");
        assert_eq!(raw["fast"]["provider"], "deepseek");
        assert_eq!(raw["apply_mode"], "manual");
        assert_eq!(raw["query_pages"], true);
    }

    /// API key 值永不落盘：配置只含变量名，把一个伪造 key 值注入环境
    /// 并走完解析路径后，db 文件字节中不得出现该值。
    #[test]
    fn api_key_value_never_persists_to_db_file() {
        let secret_value = "corpus-fake-key-do-not-persist-9f8e7d";
        std::env::set_var("CORPUS_T2_FAKE_API_KEY", secret_value);
        let (dir, store) = store();
        let mut config = AppConfig::default();
        config.fast.api_key_env = Some("CORPUS_T2_FAKE_API_KEY".into());
        store.set_config(&config).expect("保存配置失败");

        // 值确实能从环境变量解析出来（协议路径通），……
        assert_eq!(
            config.fast.resolve_api_key().expect("读取 env key 失败"),
            secret_value
        );
        // ……但 db 文件字节里绝无 key 值（连变量名带值都不含值本身）
        let mut bytes = Vec::new();
        fs::File::open(dir.path().join(CONFIG_DB_FILENAME))
            .expect("打开 db 文件失败")
            .read_to_end(&mut bytes)
            .expect("读取 db 文件失败");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(secret_value),
            "API key 值不得出现在 config.db 中"
        );
        std::env::remove_var("CORPUS_T2_FAKE_API_KEY");
    }

    #[test]
    fn reopen_keeps_config() {
        let (dir, store) = store();
        let config = AppConfig {
            query_pages: false,
            ..AppConfig::default()
        };
        store.set_config(&config).expect("保存配置失败");
        drop(store);

        let reopened = ConfigStore::open(dir.path()).expect("重开失败");
        assert_eq!(reopened.get_config().expect("读取失败"), config);
    }

    #[test]
    fn resolve_api_key_reports_missing_env_with_name() {
        let tier = ModelTier {
            provider: ProviderKind::OpenAI,
            model: "gpt-5.2".into(),
            base_url: None,
            api_key_env: Some("CORPUS_T2_DEFINITELY_UNSET_KEY".to_string()),
        };
        match tier.resolve_api_key() {
            Err(Error::MissingApiKey { env }) => {
                assert_eq!(env, "CORPUS_T2_DEFINITELY_UNSET_KEY");
            }
            other => panic!("期望 MissingApiKey，实际 {other:?}"),
        }
    }

    #[test]
    fn openai_compatible_without_env_name_is_invalid() {
        let tier = ModelTier {
            provider: ProviderKind::OpenAICompatible,
            model: "m".into(),
            base_url: Some("https://gw".into()),
            api_key_env: None,
        };
        assert!(matches!(
            tier.resolve_api_key(),
            Err(Error::InvalidConfig(_))
        ));
    }

    #[test]
    fn provider_kind_defaults() {
        assert_eq!(
            ProviderKind::OpenAI.default_api_key_env(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(ProviderKind::OpenAICompatible.default_api_key_env(), None);
        assert_eq!(ProviderKind::ALL.len(), 4);
        assert_eq!(ProviderKind::DeepSeek.as_str(), "deepseek");
    }
}
