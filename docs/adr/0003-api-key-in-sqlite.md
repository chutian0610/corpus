# API key 明文存全局 sqlite，不接系统钥匙串

用户在设置页填入的 API key 值直接存入全局 config.db（`ModelTier.api_key` 字段），不使用系统钥匙串（macOS Keychain / Secret Service / 凭据管理器）。这是对威胁模型的主动取舍：key 是可撤销、可限额的 LLM API key 而非基础设施凭据；明文本机存储换来零平台依赖（`keyring` 在无头 Linux 上会失败，而规划中的本地 web 形态恰好可能跑在无头环境）、引擎的 tempdir 可测性、以及与备份/调试一致的简单性。T2 时期"key 永不落盘"的验收语义由本决策有意取代。

## Consequences

- 解析顺序固定为：档位配置的环境变量（若存在）→ `api_key` 字段 → 报错。开发 / CI 的 export 覆盖路径不变。
- config.db 创建后须 chmod 600（unix），挡住多用户机器上的其他账户。
- 备份 / 同步会把明文 key 带走（Time Machine、目录同步）；换机 / 分享配置文件时须意识到这一点。
- 未来若要升级为钥匙串，属于取代本决策的新 ADR，不允许在实现里"顺手"改回去。
