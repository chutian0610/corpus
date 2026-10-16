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
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// config.db 文件名（位于传入目录下）。
pub const CONFIG_DB_FILENAME: &str = "config.db";

/// 配置在 `config` 表中的 key（值为整个 [`AppConfig`] 的 JSON）。
const CONFIG_KEY: &str = "app_config";

/// LLM 供应方类型：决定 provider 工厂如何构造客户端（T2 调研笔记第 2 节）。
///
/// 序列化字符串、惯例 API key 环境变量名与全集顺序**只**定义在下方的
/// [`PROVIDER_SPECS`] 单一数据源中（serde 为手写实现，从表派生）；
/// 更换供应商只改配置，引擎代码零改动。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    /// OpenAI 官方端点。
    OpenAI,
    /// Anthropic 官方端点（Messages API）。
    Anthropic,
    /// DeepSeek 官方端点（OpenAI 方言客户端）。
    DeepSeek,
    /// 任意 OpenAI 兼容端点：必须配置 base_url，工厂启用
    /// `Route::Chat` + system-instructions-as-messages 兜底。
    OpenAICompatible,
}

/// Provider 静态描述：[`ProviderKind`] 的单一数据源，新增供应方时
/// 只改这张表与 enum 本体，serde / `as_str` / `default_api_key_env`
/// / [`ProviderKind::all`] 全部自动跟随。
struct ProviderSpec {
    kind: ProviderKind,
    /// serde / 展示 / 日志用 id 字符串（如 `openai_compatible`）。
    id: &'static str,
    /// 该供应方惯例的 API key 环境变量名；`openai_compatible` 无惯例
    /// （自定义端点配自定义变量名），为 `None`。
    default_api_key_env: Option<&'static str>,
}

const PROVIDER_SPECS: &[ProviderSpec] = &[
    ProviderSpec {
        kind: ProviderKind::OpenAI,
        id: "openai",
        default_api_key_env: Some("OPENAI_API_KEY"),
    },
    ProviderSpec {
        kind: ProviderKind::Anthropic,
        id: "anthropic",
        default_api_key_env: Some("ANTHROPIC_API_KEY"),
    },
    ProviderSpec {
        kind: ProviderKind::DeepSeek,
        id: "deepseek",
        default_api_key_env: Some("DEEPSEEK_API_KEY"),
    },
    ProviderSpec {
        kind: ProviderKind::OpenAICompatible,
        id: "openai_compatible",
        default_api_key_env: None,
    },
];

impl ProviderKind {
    /// 全部供应方（顺序即 [`PROVIDER_SPECS`] 表序，展示用）。
    pub fn all() -> impl Iterator<Item = ProviderKind> {
        PROVIDER_SPECS.iter().map(|spec| spec.kind)
    }

    /// 本供应方在 [`PROVIDER_SPECS`] 中的描述项。
    fn spec(self) -> &'static ProviderSpec {
        PROVIDER_SPECS
            .iter()
            .find(|spec| spec.kind == self)
            .expect("ProviderKind 必须在 PROVIDER_SPECS 表中（新增变体请同步表）")
    }

    /// 按 id 字符串反查（serde 反序列化路径）。
    fn from_id(id: &str) -> Option<Self> {
        PROVIDER_SPECS
            .iter()
            .find(|spec| spec.id == id)
            .map(|spec| spec.kind)
    }

    /// 该供应方惯例的 API key 环境变量名；`openai_compatible` 无惯例
    /// （自定义端点配自定义变量名），返回 `None`。
    pub fn default_api_key_env(self) -> Option<&'static str> {
        self.spec().default_api_key_env
    }

    /// id 字符串（与 serde 表示一致，展示与日志用）。
    pub fn as_str(self) -> &'static str {
        self.spec().id
    }
}

/// serde 直接走 [`PROVIDER_SPECS`] 的 id，序列化结果与 `as_str` 一致。
impl Serialize for ProviderKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

/// 未知 id 报错并列出全部可用 id（同样从 [`PROVIDER_SPECS`] 派生）。
impl<'de> Deserialize<'de> for ProviderKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let id = <&str>::deserialize(deserializer)?;
        ProviderKind::from_id(id).ok_or_else(|| {
            let available = PROVIDER_SPECS
                .iter()
                .map(|spec| spec.id)
                .collect::<Vec<_>>()
                .join(", ");
            serde::de::Error::custom(format!("未知 provider：{id}（可用：{available}）"))
        })
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

impl AppConfig {
    /// 保存前即时校验：把「保存时就能发现」的配置错误挡在落盘之前，
    /// 而不是等到连通测试 / 真实调用才暴露。
    ///
    /// 规则：两档模型名必须非空；`openai_compatible` 必须配置 base_url
    /// （官方端点走默认地址，不强制）。违者报 [`Error::InvalidConfig`]，
    /// 消息指明档位与原因。
    pub fn validate(&self) -> Result<()> {
        for (label, tier) in [
            ("快速档（fast）", &self.fast),
            ("强力档（strong）", &self.strong),
        ] {
            if tier.model.trim().is_empty() {
                return Err(Error::InvalidConfig(format!(
                    "{label}模型名未填写（provider：{}）",
                    tier.provider.as_str()
                )));
            }
            if tier.provider == ProviderKind::OpenAICompatible
                && tier
                    .base_url
                    .as_deref()
                    .map(str::trim)
                    .unwrap_or("")
                    .is_empty()
            {
                return Err(Error::InvalidConfig(format!(
                    "{label}使用 openai_compatible 端点时必须填写 base_url"
                )));
            }
        }
        Ok(())
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
    /// 锁住连接；持锁线程 panic 意味着进程已处于未定义状态，视为进程级
    /// bug 直接 panic（统一信息，全部读写入口只经本辅助取锁）。
    pub(crate) fn lock_conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().expect("config db 连接锁中毒")
    }

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
        let conn = self.lock_conn();
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
    ///
    /// 落盘前先做 [`AppConfig::validate`] 即时校验：模型名空缺、兼容端点
    /// 缺 base_url 等本地可判定的错误在保存时即报出，不进库。
    pub fn set_config(&self, config: &AppConfig) -> Result<()> {
        config.validate()?;
        let json = serde_json::to_string(config)?;
        let conn = self.lock_conn();
        conn.execute(
            "INSERT INTO config (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (CONFIG_KEY, json),
        )?;
        Ok(())
    }
}

/// 测试辅助（config 与 registry 两处单测共用）：临时目录下打开 store。
///
/// TempDir 必须与 ConfigStore 一同存活——目录被删后连接写入会报
/// readonly database，故返回值持有两者。
#[cfg(test)]
impl ConfigStore {
    pub(crate) fn test_open() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().expect("创建 tempdir 失败");
        let store = ConfigStore::open(dir.path()).expect("打开 ConfigStore 失败");
        (dir, store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::GENERIC;
    use std::io::Read;

    /// 通过 set_config 校验的合法配置（改动字段后仍须整体合法）。
    fn valid_config() -> AppConfig {
        AppConfig {
            fast: ModelTier {
                provider: ProviderKind::OpenAI,
                model: "gpt-5.2".into(),
                base_url: None,
                api_key_env: None,
            },
            strong: ModelTier {
                provider: ProviderKind::Anthropic,
                model: "claude-sonnet-4-6".into(),
                base_url: None,
                api_key_env: None,
            },
            apply_mode: ApplyMode::Manual,
            query_pages: true,
        }
    }

    #[test]
    fn default_config_when_db_empty() {
        let (_dir, store) = ConfigStore::test_open();
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
        let (_dir, store) = ConfigStore::test_open();
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
        let (dir, store) = ConfigStore::test_open();
        let config = AppConfig {
            fast: ModelTier {
                provider: ProviderKind::DeepSeek,
                model: "deepseek-v4-flash".into(),
                base_url: None,
                api_key_env: None,
            },
            ..valid_config()
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

    /// 保存时即时校验：空模型名 / 兼容端点缺 base_url 拒绝落盘，
    /// 错误消息指明档位与原因。
    #[test]
    fn set_config_rejects_empty_model_name() {
        let (_dir, store) = ConfigStore::test_open();
        let mut config = valid_config();
        config.strong.model = "   ".into();
        let err = store.set_config(&config).expect_err("空模型名应被拒绝");
        assert!(
            matches!(err, Error::InvalidConfig(ref msg) if msg.contains("strong") && msg.contains("模型名")),
            "实际错误：{err}"
        );
        // 校验失败不得留下半成品：库中仍无配置记录，读回默认值
        assert_eq!(store.get_config().expect("读取失败"), AppConfig::default());
    }

    #[test]
    fn set_config_rejects_compatible_endpoint_without_base_url() {
        let (_dir, store) = ConfigStore::test_open();
        let mut config = valid_config();
        config.fast = ModelTier {
            provider: ProviderKind::OpenAICompatible,
            model: "qwen3-max".into(),
            base_url: None,
            api_key_env: Some("GW_API_KEY".into()),
        };
        let err = store.set_config(&config).expect_err("缺 base_url 应被拒绝");
        assert!(
            matches!(err, Error::InvalidConfig(ref msg) if msg.contains("fast") && msg.contains("base_url")),
            "实际错误：{err}"
        );
    }

    #[test]
    fn validate_accepts_official_endpoints_without_base_url() {
        // 官方端点（openai/anthropic/deepseek）base_url 可选：不填即合法
        for provider in ProviderKind::all().filter(|p| *p != ProviderKind::OpenAICompatible) {
            let mut config = valid_config();
            config.fast = ModelTier {
                provider,
                model: "m".into(),
                base_url: None,
                api_key_env: None,
            };
            config
                .validate()
                .unwrap_or_else(|e| panic!("{provider:?} 官方端点应免 base_url：{e}"));
        }
    }

    /// API key 值永不落盘：配置只含变量名，把一个伪造 key 值注入环境
    /// 并走完解析路径后，db 文件字节中不得出现该值。
    #[test]
    fn api_key_value_never_persists_to_db_file() {
        let secret_value = "corpus-fake-key-do-not-persist-9f8e7d";
        std::env::set_var("CORPUS_T2_FAKE_API_KEY", secret_value);
        let (dir, store) = ConfigStore::test_open();
        let mut config = valid_config();
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
        let (dir, store) = ConfigStore::test_open();
        let config = AppConfig {
            query_pages: false,
            ..valid_config()
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

    /// 注册表读写同库：配置保存后 recent_vaults 表仍可用（跨模块联动冒烟）。
    #[test]
    fn config_and_registry_share_one_db() {
        let (_dir, store) = ConfigStore::test_open();
        let config = AppConfig {
            apply_mode: ApplyMode::Auto,
            ..valid_config()
        };
        store.set_config(&config).expect("保存配置失败");
        store
            .add_recent_vault("/tmp/some-vault", &GENERIC.id)
            .expect("添加注册表失败");

        assert_eq!(store.get_config().expect("读配置失败"), config);
        assert_eq!(store.list_recent_vaults().expect("列注册表失败").len(), 1);
    }

    /// 单一数据源派生的一致性：as_str / default_api_key_env / serde
    /// 字符串三者互通，全集与顺序来自 PROVIDER_SPECS。
    #[test]
    fn provider_kind_table_is_single_source_of_truth() {
        assert_eq!(ProviderKind::all().count(), 4);
        assert_eq!(ProviderKind::DeepSeek.as_str(), "deepseek");
        assert_eq!(
            ProviderKind::OpenAI.default_api_key_env(),
            Some("OPENAI_API_KEY")
        );
        assert_eq!(ProviderKind::OpenAICompatible.default_api_key_env(), None);

        // serde 手写实现与 as_str 一致：全部 id 可 JSON 往返
        for kind in ProviderKind::all() {
            let json = serde_json::to_string(&kind).expect("序列化失败");
            let back: ProviderKind = serde_json::from_str(&json).expect("反序列化失败");
            assert_eq!(back, kind);
        }

        // 未知 id 报错并列出可用 id
        let err =
            serde_json::from_str::<ProviderKind>("\"nope\"").expect_err("未知 provider 应报错");
        let msg = err.to_string();
        assert!(msg.contains("nope"));
        assert!(msg.contains("openai_compatible"));
    }
}
