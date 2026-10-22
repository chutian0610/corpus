# API key 明文存全局 sqlite，不接系统钥匙串

用户在设置页填入的 API key 值直接存入全局 config.db（`ModelTier.api_key` 字段），不使用系统钥匙串（macOS Keychain / Secret Service / 凭据管理器）。这是对威胁模型的主动取舍：key 是可撤销、可限额的 LLM API key 而非基础设施凭据；明文本机存储换来零平台依赖（`keyring` 在无头 Linux 上会失败，而规划中的本地 web 形态恰好可能跑在无头环境）、引擎的 tempdir 可测性、以及与备份/调试一致的简单性。T2 时期"key 永不落盘"的验收语义由本决策有意取代。

## Consequences

- ~~解析顺序固定为：档位配置的环境变量（若存在）→ `api_key` 字段 → 报错。开发 / CI 的 export 覆盖路径不变。~~（**已废止**，见下方 2026-10-21 修订：环境变量路径整体移除。）
- config.db 创建后须 chmod 600（unix），挡住多用户机器上的其他账户。
- 备份 / 同步会把明文 key 带走（Time Machine、目录同步）；换机 / 分享配置文件时须意识到这一点。
- 未来若要升级为钥匙串，属于取代本决策的新 ADR，不允许在实现里"顺手"改回去。

## 修订（2026-10-21）：环境变量路径整体移除

环境变量机制整体移除（`api_key_env` 字段、`PROVIDER_SPECS` 的惯例环境变量名列、解析顺序中的 env 一步、`CORPUS_HOME/.env` 自动加载机制均废止）——那是给已废弃的"key 永不落盘"姿态续命的残留，且本软件未真正发布过、无外部用户依赖 env 注入路径。修订后 **API key 唯一来源 = 设置页输入 → config.db 明文**，`resolve_api_key` 只读配置字段，缺失即报错并指路设置页。server 的 `GET /api/config` 会把 `api_key` 明文一并返回：本机信任域（仅绑 127.0.0.1）+ 一次性 token 门控，属本决策接受的取舍。
