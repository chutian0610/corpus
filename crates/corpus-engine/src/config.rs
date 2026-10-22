//! 全局配置层：应用数据目录下 `config.db`（sqlite）中的模型与行为配置。
//!
//! 设计约束（见 T2 调研笔记第 5、8 节）：
//! - rusqlite（bundled）同步 API + `Mutex<Connection>` 包裹——SQLite 单文件
//!   低并发的本质与同步模型匹配，壳层（Tauri command）已在线程池执行；
//! - **API key 以明文值存本机 config.db**（[`ModelTier::api_key`]，ADR-0003
//!   的主动取舍）：设置页输入是唯一来源，环境变量 / `.env` 路径已整体移除；
//! - 引擎只接受目录参数（[`ConfigStore::open`]），`tempfile::tempdir()` 即可测试。
//!
//! Vault 注册表（最近打开）与配置同库，见 [`crate::registry`]。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use rusqlite::Connection;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::error::{Error, Result};

/// config.db 文件名（位于传入目录下）。
pub const CONFIG_DB_FILENAME: &str = "config.db";

/// config.db 权限收紧为 `0600`（unix，ADR-0003 补偿措施 2）：新建文件
/// 的 umask 默认权限不可靠，显式 set_permissions 挡住多用户机器上其他
/// 账户读取。非 unix（Windows 走 ACL，无此语义）为空操作，直接跳过。
#[cfg(unix)]
fn restrict_db_permissions(db_path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(db_path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

/// 非 unix 平台无 chmod 语义（Windows 文件权限走 ACL），不做任何事。
#[cfg(not(unix))]
fn restrict_db_permissions(_db_path: &Path) -> Result<()> {
    Ok(())
}

/// 配置在 `config` 表中的 key（值为整个 [`AppConfig`] 的 JSON）。
const CONFIG_KEY: &str = "app_config";

/// LLM 供应方类型：决定 provider 工厂如何构造客户端（T2 调研笔记第 2 节）。
///
/// 序列化字符串与全集顺序**只**定义在下方的 [`PROVIDER_SPECS`] 单一数据源中
/// （serde 为手写实现，从表派生）；更换供应商只改配置，引擎代码零改动。
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
    /// MiniMax 官方端点（rig 原生方言，OpenAI 格式线）：默认国内站
    /// `api.minimaxi.com`，国际站经 base_url 覆盖为
    /// `https://api.minimax.io/v1`。M3 系列为推理模型，工厂按档位附加
    /// reasoning 参数（见 ingest 的 fast 档端口构造）。
    Minimax,
}

/// Provider 静态描述：[`ProviderKind`] 的单一数据源，新增供应方时
/// 只改这张表与 enum 本体，serde / `as_str` / [`ProviderKind::all`]
/// 全部自动跟随。
struct ProviderSpec {
    kind: ProviderKind,
    /// serde / 展示 / 日志用 id 字符串（如 `openai_compatible`）。
    id: &'static str,
}

const PROVIDER_SPECS: &[ProviderSpec] = &[
    ProviderSpec {
        kind: ProviderKind::OpenAI,
        id: "openai",
    },
    ProviderSpec {
        kind: ProviderKind::Anthropic,
        id: "anthropic",
    },
    ProviderSpec {
        kind: ProviderKind::DeepSeek,
        id: "deepseek",
    },
    ProviderSpec {
        kind: ProviderKind::OpenAICompatible,
        id: "openai_compatible",
    },
    ProviderSpec {
        kind: ProviderKind::Minimax,
        id: "minimax",
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
/// `api_key` 是 key **明文值**而非环境变量名（ADR-0003 的主动取舍）：
/// 设置页输入是唯一来源，落盘本机 config.db（0600），无环境变量回退。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTier {
    /// 供应方类型。
    pub provider: ProviderKind,
    /// 模型名（如 `gpt-5.2`、`claude-sonnet-4-6`）；空串表示尚未配置。
    pub model: String,
    /// 自定义 base_url（OpenAI / Anthropic / OpenAI 兼容端点可选或必填）。
    pub base_url: Option<String>,
    /// API key 明文值（ADR-0003：存本机 config.db，0600 权限补偿）；
    /// `None` / 空表示尚未配置，该档位无法调用。
    pub api_key: Option<String>,
}

impl ModelTier {
    /// 解析实际使用的 API key 值：直接取配置中的明文字段克隆。
    ///
    /// 未配置（`None` 或 trim 后为空）报 [`Error::MissingApiKey`]——
    /// 不存在环境变量查找，缺 key 的唯一出路是去设置页填写。
    pub fn resolve_api_key(&self) -> Result<String> {
        match self.api_key {
            Some(ref key) if !key.trim().is_empty() => Ok(key.clone()),
            _ => Err(Error::MissingApiKey),
        }
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
    /// 给出明确错误），api_key 未配置（`None`）。
    fn default_for(provider: ProviderKind) -> Self {
        Self {
            provider,
            model: String::new(),
            base_url: None,
            api_key: None,
        }
    }
}

/// 全局数据目录解析：`CORPUS_HOME` 环境变量优先，未设置（或为空）时
/// 落回壳层给定的平台默认（桌面 = Tauri `app_data_dir`；server =
/// 系统数据目录下的 `dev.corpus.desktop`）。
///
/// 设了 `CORPUS_HOME` 就创建该目录——e2e / 沙盒常用全新临时目录，
/// 调用方不必再判断存在性。config.db（配置 + Vault 注册表）跟随本目录；
/// Vault 内的 `.corpus/`（指纹缓存、llm-calls.log）不在此列，本就
/// 随 Vault 自包含。
///
/// 背景：shell 层的 e2e 曾因与桌面壳共用真实 config.db 而覆写用户
/// 配置——本环境变量是数据面隔离的正式机制，任何起 server / 写
/// 数据面的 e2e 都必须设 `CORPUS_HOME` 指向临时目录。
pub fn corpus_home(fallback: impl AsRef<Path>) -> PathBuf {
    match std::env::var("CORPUS_HOME") {
        Ok(home) if !home.trim().is_empty() => {
            let dir = PathBuf::from(home.trim());
            if let Err(err) = fs::create_dir_all(&dir) {
                // 建目录失败不在此处中断：ConfigStore::open 会以更明确的
                // 错误失败，这里只保留路径语义。
                eprintln!("CORPUS_HOME 目录创建失败（{}）：{err}", dir.display());
            }
            dir
        }
        _ => fallback.as_ref().to_path_buf(),
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
    ///
    /// 文件权限（unix，ADR-0003 补偿措施 2 / 票据 #18 验收项）：无论新建
    /// 还是打开既有库，一律把 config.db 收紧为 `0600`——sqlite 新建文件
    /// 按 umask 落盘（常见 0644，组 / 其他可读），显式 chmod 挡住多用户
    /// 机器上其他账户；旧版本遗留的宽松权限在再次打开时一并纠正。
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir)?;
        let db_path = dir.join(CONFIG_DB_FILENAME);
        let conn = Connection::open(&db_path)?;
        restrict_db_permissions(&db_path)?;
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
                api_key: None,
            },
            strong: ModelTier {
                provider: ProviderKind::Anthropic,
                model: "claude-sonnet-4-6".into(),
                base_url: None,
                api_key: None,
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
        assert_eq!(config.fast.api_key, None);
    }

    #[test]
    fn set_then_get_roundtrip_via_public_api() {
        let (_dir, store) = ConfigStore::test_open();
        let config = AppConfig {
            fast: ModelTier {
                provider: ProviderKind::OpenAICompatible,
                model: "qwen3-max".into(),
                base_url: Some("https://gw.example.com/v1".into()),
                api_key: Some("gw-plaintext-key".into()),
            },
            strong: ModelTier {
                provider: ProviderKind::Anthropic,
                model: "claude-sonnet-4-6".into(),
                base_url: None,
                api_key: None,
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
                api_key: Some("ds-plaintext-key".into()),
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
        // 明文姿态（ADR-0003）：api_key 字段原样进 JSON
        assert_eq!(raw["fast"]["api_key"], "ds-plaintext-key");
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
            api_key: Some("gw-plaintext-key".into()),
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
                api_key: None,
            };
            config
                .validate()
                .unwrap_or_else(|e| panic!("{provider:?} 官方端点应免 base_url：{e}"));
        }
    }

    /// 明文姿态（ADR-0003）：api_key Some → resolve 原样返回该值；
    /// 配置落盘后 db 文件字节里真实包含该明文（与旧「永不落盘」语义相反，
    /// 有意断言正向事实，防止实现悄悄绕开 config.db）。
    #[test]
    fn api_key_resolves_from_config_and_persists_plaintext_to_db_file() {
        let secret_value = "corpus-plaintext-key-roundtrip-9f8e7d";
        let (dir, store) = ConfigStore::test_open();
        let mut config = valid_config();
        config.fast.api_key = Some(secret_value.into());
        store.set_config(&config).expect("保存配置失败");

        // resolve 直接来自配置字段（无 env 查找路径）
        assert_eq!(
            config.fast.resolve_api_key().expect("解析 api_key 失败"),
            secret_value
        );
        // 重开后仍解析出同一值（真实从 db 读回）
        let reopened = ConfigStore::open(dir.path()).expect("重开失败");
        let loaded = reopened.get_config().expect("读取配置失败");
        assert_eq!(
            loaded.fast.resolve_api_key().expect("重开解析失败"),
            secret_value
        );
        // db 文件字节里就是明文
        let mut bytes = Vec::new();
        fs::File::open(dir.path().join(CONFIG_DB_FILENAME))
            .expect("打开 db 文件失败")
            .read_to_end(&mut bytes)
            .expect("读取 db 文件失败");
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains(secret_value),
            "api_key 明文应出现在 config.db 中（ADR-0003 姿态）"
        );
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

    /// ADR-0003 补偿措施 2（票据 #18 验收项提前落地）：open 新建
    /// config.db 后权限必须是 0600（挡住多用户机器上其他账户读取）。
    #[cfg(unix)]
    #[test]
    fn open_restricts_new_config_db_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _store) = ConfigStore::test_open();
        let mode = fs::metadata(dir.path().join(CONFIG_DB_FILENAME))
            .expect("config.db 应存在")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "config.db 应为 0600，实际 {:o}",
            mode & 0o777
        );
    }

    /// 旧库遗留的宽松权限（如 0644）在再次 open 时一并纠正为 0600。
    #[cfg(unix)]
    #[test]
    fn open_corrects_preexisting_loose_config_db_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, store) = ConfigStore::test_open();
        let db = dir.path().join(CONFIG_DB_FILENAME);
        fs::set_permissions(&db, fs::Permissions::from_mode(0o644)).expect("放宽权限失败");
        drop(store);

        ConfigStore::open(dir.path()).expect("重开失败");
        let mode = fs::metadata(&db)
            .expect("config.db 应存在")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "旧库宽松权限应在打开时纠正，实际 {:o}",
            mode & 0o777
        );
    }

    /// 未配置 key（None）报 MissingApiKey，文案指向设置页与 ADR-0003
    /// （不再有 env 变量名，也不再有回退查找）。
    #[test]
    fn resolve_api_key_reports_missing_key_with_setup_guidance() {
        let tier = ModelTier {
            provider: ProviderKind::OpenAI,
            model: "gpt-5.2".into(),
            base_url: None,
            api_key: None,
        };
        match tier.resolve_api_key() {
            Err(err @ Error::MissingApiKey) => {
                let msg = err.to_string();
                assert!(msg.contains("设置页"), "文案应指路设置页：{msg}");
                assert!(msg.contains("config.db"), "文案应指明存储位置：{msg}");
                assert!(msg.contains("ADR-0003"), "文案应引用 ADR：{msg}");
            }
            other => panic!("期望 MissingApiKey，实际 {other:?}"),
        }
    }

    /// 空串与纯空白等同未配置：同样报 MissingApiKey（前端提交已 trim
    /// 空转 null，此处防御手改 db / 旧库残留）。
    #[test]
    fn resolve_api_key_treats_blank_as_missing() {
        for blank in [Some("".to_string()), Some("   ".to_string())] {
            let tier = ModelTier {
                provider: ProviderKind::OpenAICompatible,
                model: "m".into(),
                base_url: Some("https://gw".into()),
                api_key: blank,
            };
            assert!(
                matches!(tier.resolve_api_key(), Err(Error::MissingApiKey)),
                "空白 api_key 应报 MissingApiKey"
            );
        }
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

    /// 单一数据源派生的一致性：as_str / serde 字符串两者互通，全集与
    /// 顺序来自 PROVIDER_SPECS。
    #[test]
    fn provider_kind_table_is_single_source_of_truth() {
        assert_eq!(ProviderKind::all().count(), 5);
        assert_eq!(ProviderKind::DeepSeek.as_str(), "deepseek");
        assert_eq!(ProviderKind::Minimax.as_str(), "minimax");

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

#[cfg(test)]
mod corpus_home_tests {
    use super::*;

    /// env 是进程全局态：并发测试下用互斥锁串行化对本变量的读写。
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn env_set_creates_and_wins_over_fallback() {
        let _guard = ENV_LOCK.lock().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("CORPUS_HOME", home.path());
        let fallback = home.path().join("fallback-should-not-be-used");

        let resolved = corpus_home(&fallback);

        assert_eq!(resolved, home.path());
        assert!(home.path().is_dir(), "设了 CORPUS_HOME 应自动建目录");
        assert!(!fallback.exists(), "fallback 不应被触碰");
        std::env::remove_var("CORPUS_HOME");
    }

    #[test]
    fn env_unset_or_blank_falls_back_without_creation() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("CORPUS_HOME");
        let fallback = tempfile::tempdir().unwrap().path().join("not-created");

        assert_eq!(corpus_home(&fallback), fallback);
        assert!(
            !fallback.exists(),
            "未设 env 不应创建 fallback（创建是壳层职责）"
        );

        // 空串与纯空白等同未设置
        std::env::set_var("CORPUS_HOME", "   ");
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(corpus_home(dir.path()), dir.path());
        std::env::remove_var("CORPUS_HOME");
    }
}
