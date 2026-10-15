# T2 前置调研笔记：Rig 接入、全局 sqlite 配置、Tauri 2 目录（2026-10-15）

所有 rig API 均以 **rig 0.44.0（2026-10-07 发布，MIT）** 为准。注意：v0.44 是大破坏版本（PR 跨度 #2372→#2640，官方自称移除了全部向后兼容 shim），README 上的版本示例还写着 `0.36.0` 属于文档滞后，API 形态以 docs.rs 0.44 与 rig.rs 文档为准。

## 1. 依赖引入：`rig` 门面 vs `rig-core`

**事实**（docs.rs/rig）：`rig` 是 facade crate——"Public facade for Rig."，把 rig-core 在 `rig::...` 路径 re-export；开 `agent` feature（默认开）时把 rig-agent 暴露为 `rig::agent`。只要核心 provider 抽象可直依赖 `rig-core`。仓库根 Cargo.toml 即 facade，workspace 31 个成员 crate 在 `crates/` 下（rig-core、rig-agent、rig-reqwest、rig-cassette 等）。

**facade 的 default features**（根 Cargo.toml 原文）：

```toml
default = ["rig-core/default", "reqwest", "agent", "derive", "rustls"]
```

v0.44 关键架构变化：rig-reqwest 拆为独立 crate，**rig-core 自身无默认传输、不含 reqwest/tokio**（v0.44.0 release notes #2397/#2398）。所有 `Xxx::new(api_key)` / `from_env()` 构造函数标注"仅 `reqwest` feature"。**结论：桌面应用直接用 `rig` 门面（默认 features 即含 reqwest+agent+rustls），不要裸用 rig-core。**

**workspace 锁版本写法**（rig 仓库自身用法：path + 精确 version）：

```toml
[workspace.dependencies]
rig = "=0.44.0"   # 0.x 频繁破坏性变更，建议 = 精确锁 + Cargo.lock 提交
# corpus-engine 里：
# rig = { workspace = true }
```

cassette（录制回放，见第 4 节）经门面启用：`rig = { version = "0.44", features = ["cassette"] }`（re-export 为 `rig::cassette`）。

## 2. Provider 构造（0.44 API，均需 `reqwest` feature）

**OpenAI（含任意兼容端点）**（docs.rs OpenAI / OpenAIConfig、rig.rs 官方 OpenAI 集成文档，可直接照抄）：

```rust
use rig::providers::openai::{self, OpenAI, OpenAIConfig, Route};

// 环境变量：OPENAI_API_KEY + 可选 OPENAI_BASE_URL
let client = OpenAI::from_env()?;
// 显式 key，官方端点
let client = OpenAI::new("your-api-key");
// 自定义/兼容端点（T2 的 OpenAI 兼容网关走这里）
let client = OpenAIConfig::new("your-api-key")
    .with_base_url("https://your-gateway.example.com/v1")
    .client();          // 或 .connect(http: impl HttpClientExt) 换传输

// 强制 Chat Completions 线路（默认走 Responses API）：
let model = OpenAIConfig::from_env()?
    .with_route(Route::Chat)
    .client()
    .completion(openai::GPT_5_2);
```

模型方法：`completion(model)` / `responses(model)` / `chat(model)` / `embedding(model, ndims)`，另有 `verify()`（401/403 → `ProviderError::InvalidAuthentication`，可用于启动时校验 key）与 `list_models()`。

**Anthropic**（docs.rs Anthropic / AnthropicConfig）：

```rust
use rig::providers::anthropic;

let provider = anthropic::Anthropic::from_env()?;   // ANTHROPIC_API_KEY / ANTHROPIC_BASE_URL
// 或显式：
let provider = anthropic::Anthropic::new("api-key");
// 或自定义端点：
let provider = anthropic::AnthropicConfig::new("api-key")
    .with_base_url("https://gw.example.com")        // 文档注明 "without a trailing /v1"
    .client();
let sonnet = provider.completion(anthropic::completion::CLAUDE_SONNET_4_6);
```

**DeepSeek**（docs.rs deepseek）：DeepSeek 是 **OpenAI 方言**——`deepseek::new` 的真实签名是 `pub fn new(api_key: impl Into<Secret>) -> OpenAI`（返回预配置 DEEPSEEK dialect/base_url 的 OpenAI 型客户端）：

```rust
use rig::providers::deepseek;

let model = deepseek::from_env()?          // 或 deepseek::new("api-key")
    .chat(deepseek::DEEPSEEK_V4_FLASH);    // 常量：DEEPSEEK_V4_FLASH / DEEPSEEK_V4_PRO / DEEPSEEK_FLASH
```

## 3. 一次最小补全：agent 构造 + prompt + 流式 + system prompt

**非流式 + 流式**（Agent struct 文档示例原样、README）：

```rust
use rig::prelude::*;
use rig::providers::openai::{self, OpenAI};

#[tokio::main]   // 需 tokio features = ["macros", "rt-multi-thread"] 或 "full"
async fn main() -> Result<(), anyhow::Error> {
    let model = OpenAI::from_env()?.completion(openai::GPT_5_2);
    let agent = AgentBuilder::new(model)                    // impl Into<DynModel<Completion>>
        .preamble("You are a comedian here to entertain the user.")  // system prompt 注入点
        .temperature(0.7)                                    // 可选：max_tokens/stop/output_schema...
        .build();

    // 非流式：prompt() 返回 AgentRunner，"nothing happens until it is driven"
    let response = agent.prompt("Entertain me!").max_turns(3).await?;
    println!("{}", response.output());                      // PromptResponse::output() -> String

    // 流式：
    let mut stream = agent.prompt("And 3 + 3?").stream();
    while let Some(item) = stream.next().await {
        let item = item?;   // MultiTurnStreamItem：text deltas、tool calls、FinalResponse
    }
    Ok(())
}
```

关键形态（0.44）：`agent.prompt(msg)` 返回 `AgentRunner`，`.await` 折叠为 `PromptResponse`，`.stream()` 产出 `MultiTurnStreamItem` 流；system prompt 在 **AgentBuilder 层用 `.preamble()`**（`without_preamble()` 清除、`append_preamble()` 追加）。

**`with_system_instructions_as_messages` 挂在 `OpenAIConfig` 层**（rig.rs 官方 OpenAI 文档原文：Responses 路由会把 agent 的 preamble 作为顶层 `instructions` 字段发送；"For a compatible backend that ignores or rejects that field, `OpenAIConfig::with_system_instructions_as_messages()` sends it as system messages in the input"）：

```rust
let client = OpenAIConfig::new(key)
    .with_base_url("https://compat-endpoint/v1")
    .with_system_instructions_as_messages()   // 无参，OpenAIConfig 层；preamble 改走 input 里的 system 消息
    .client();
```

注意：docs.rs 的 OpenAIConfig 方法列表未展示此方法（crate 文档覆盖率 55.56%），以 rig.rs 官方文档（对应 0.44）为准；其底层字段为 `OpenAIConfig.system_instructions: Option<SystemInstructionsPlacement>`（如 OpenRouter 方言默认 `SystemInstructionsPlacement::InputSystemMessages`，见 dialects.rs 源码）。**结论：走兼容端点时建议一并设 `Route::Chat` 或 `with_system_instructions_as_messages()` 兜底。**

## 4. 测试替身

- **rig 0.44 没有面向用户的稳定 fake trait**。rig::completion 模块只有 `ProviderExtension`、`ReplayTarget`、`ExtensionOptions`、`ReplyExtras`——经典 `CompletionModel`/`Model` trait 不在公开 API 中。动态分发点是 prelude 里的 `DynModel`（"A model of one operation with its wire and transport erased"，经 `Model::erase` / `From<Model<W,T>>` 构造），但自造 fake 需实现底层 Operation/Wire/Decoder/Transport 契约，v0.44 这套刚重构（"客户端机制收缩为 Provider + Has*"），成本高。
- **rig-cassette 0.44.0**（docs.rs，100% documented，随 rig 同版本发布）：HTTP 录制/回放。回放是默认行为，`RIG_PROVIDER_TEST_MODE=record` 时联系真 provider 并覆盖脱敏后的 fixture；核心类型 `ProviderCassette`（`start_at`/`finish`/`try_start_at`/`try_finish`）、`CassetteSpec`、`DirectRecordingHttpClient`；经 `rig` 门面 `features = ["cassette"]` 使用（tests/README.md 有完整环境变量与 `cargo xtask cassette record` 工作流）。它主要服务 rig 仓库自身的 18 家 provider wire 测试（fixtures、脱敏、账号失败拒绝录制），**面向 HTTP 线路断言，不是引擎逻辑的 mock 框架**，下游可用但偏重。
- **佐证结论**：rig 未提供稳定的公开 mock trait（completion 模块文档可证）+ 0.x 每 minor 均破坏（MIGRATING.md 可证）→ T2 在 corpus-engine 内自定义窄 trait（如 `trait LlmPort { async fn complete(&self, req: CompletionRequest) -> Result<CompletionOutput>; }`），rig Agent 包在实现里、feature-gated，测试用手写 Fake——这是隔离 rig 破坏性变更的常规防线。

## 5. sqlite 选型：rusqlite（bundled）

- **rusqlite 当前版本 0.40.2**（docs.rs，2026-10-12 构建；README 以 0.40.1 为例，bundled 内嵌 SQLite 3.53.2）。
- **bundled**（rusqlite README）：用 `cc` 从内嵌 amalgamation 编译 SQLite 并静态链接，"avoids depending on the version of SQLite on the users system"。启用写法：

```toml
[dependencies]
rusqlite = { version = "0.40", features = ["bundled"] }
```

README 提醒："should probably not enable it" 是对**发布的通用库**说的；corpus-engine 是最终应用的内部 crate，直接启用没有问题（否则需由 app crate 透传 feature）。
- **vs sqlx/diesel**：rusqlite 同步、零服务依赖、体积小，与 SQLite 单文件低并发的本质匹配；SQLite 没有非阻塞 API，async 框架（sqlx）只是线程池包阻塞调用，有显著开销（sqlx 性能讨论 issue、Rustfinity 对比）；diesel 是类型安全 DSL 但重、编译慢，T2 只有配置表+注册表两张表用不上。**推荐 rusqlite + bundled**；若担心同步阻塞 UI，在引擎层用 `std::sync::Mutex<Connection>` 包一层即可（Tauri command 已在线程池跑）。

## 6. Tauri 2 应用数据目录与传给引擎

- **官方 API**（tauri::path::PathResolver）：`pub fn app_data_dir(&self) -> Result<PathBuf>`（`tauri::Result`），解析为 `data_dir()/${bundle_identifier}`，可被配置 `app > appDirectoriesOverride` 覆盖。Rust 侧在 `setup` 闭包经 `app.path().app_data_dir()` 获取；目录不存在需自己 `std::fs::create_dir_all`。注意 MSIX 打包环境与开发期路径可能不同（tauri issue #9936），必须运行时解析、不能硬编码。
- **传给 UI 无关引擎的社区共识模式**：core crate 不依赖 tauri，构造函数接受 `PathBuf`，Tauri 层注入并 `manage`：

```rust
tauri::Builder::default()
    .setup(|app| {
        let dir = app.path().app_data_dir()?;   // tauri::Manager trait
        std::fs::create_dir_all(&dir)?;
        let engine = corpus_engine::Engine::open(&dir)?;   // 引擎只认目录参数 → tempfile::tempdir() 可测
        app.manage(engine);
        Ok(())
    })
```

corpus-engine 里所有持久化入口（`ConfigStore::open(dir)`、`VaultRegistry::open(dir)`）签名收 `impl AsRef<Path>`，保证 tempdir 测试。

## 7. 已知坑（v0.44 迁移要点，摘自 MIGRATING.md 与 v0.44.0 release notes）

- `agent.prompt(p).await?.output`（字段）→ `.output()`（方法，返回 owned String）。
- `PromptError` 变体大改名：`CompletionError`→`Provider`、`MemoryError`→`Memory`、`MaxTurnsError`→`MaxTurns`、`PromptCancelled`→`Cancelled`；`StreamingError` 移除（流式与 awaited 共用 `PromptError`）。
- `CompletionRequest` 等禁止 struct literal，改 `CompletionRequest::new(prompt)` + setters；缓存统一 `.cache(CacheRetention::Short/Long)`。
- **未知 finish reason / 被过滤文本现在直接失败运行**（不再静默），可用 `AgentBuilder::accept_unknown_finish_reasons(true)` 放宽——对接第三方兼容端点时建议评估。
- rig-reqwest/rig-rmcp 拆分：rig-core 无默认传输、不含 reqwest/tokio；`OpenAI::new` 等"shared reqwest client"构造全部要求 `reqwest` feature（`rig` 门面默认已含）。
- 全 workspace 移除 `wasm` feature flag（按 target 判断）；移除全部向后兼容 shim；`Vec` 替换 `NonEmpty`；provider 类型化 request/reply struct 改为 `extension::{…Options, …Extras}`。
- 0.43 持久化的 `AgentRun`（format 1）在 0.44 不可加载（"this rig reads format 2"）。
- **rusqlite bundled 编译要求**：任何 bundled feature 需要"较新的 Clang"（libsqlite3-sys），由 cc crate 编译 `sqlite3.c`；macOS arm64 无特殊要求，装好 Xcode Command Line Tools（提供 clang）即可，无额外系统依赖。

## 8. 推荐路径结论

**依赖清单**（corpus workspace 根）：

```toml
[workspace.dependencies]
rig       = "=0.44.0"                                        # 门面；默认 features（reqwest+agent+derive+rustls）够用
rusqlite  = { version = "0.40.2", features = ["bundled"] }    # 同步、单文件、免系统 sqlite
serde     = { version = "1", features = ["derive"] }
thiserror = "2"
# dev-dependencies: tempfile（tempdir 测试）、tokio（rt/macros）
# 可选 dev: rig = { features = ["cassette"] }（provider 线路回放）
```

tauri app crate 才依赖 `tauri = "2"`；**corpus-engine 零 tauri 依赖**。

**引擎 API 形态建议**：

1. `corpus-engine` 内定义窄端口 trait（如 `LlmPort: Send + Sync`，方法 `complete(system: Option<&str>, user: &str, opts) -> Result<String>` 与 `stream(...) -> impl Stream`），rig 实现放 `rig` feature 后面；系统 prompt 由引擎层透传给 `AgentBuilder::preamble()`。OpenAI 兼容端点统一走 `OpenAIConfig::new(key).with_base_url(url).client()`，并按端点能力选择 `with_route(Route::Chat)` / `with_system_instructions_as_messages()` 兜底。测试用手写 Fake LlmPort（rig 无稳定公开 mock trait，见第 4 节）。
2. Provider 工厂签名建议 `build(kind: ProviderKind, api_key_env: &str, base_url: Option<&str>) -> Result<DynLlm>`；三家构造：OpenAI `OpenAIConfig::new` / Anthropic `AnthropicConfig::new` / DeepSeek `deepseek::new`（返回 DEEPSEEK 方言的 OpenAI client，走 `.chat(...)`）。
3. `ConfigStore::open(impl AsRef<Path>)` 打开 `{dir}/config.db`（rusqlite bundled，`Mutex<Connection>` 包裹），表：双模型档位（fast/strong：provider/model/base_url）、apply 模式、query 沉淀开关；`VaultRegistry` 同库或同目录。API key 一律从环境变量读（`OPENAI_API_KEY`/`ANTHROPIC_API_KEY`/自定义 env 名存配置表，值不落盘）。
4. Tauri 层只做一件事：`app.path().app_data_dir()?` + `create_dir_all` + 注入 `Engine::open(dir)` 并 `app.manage`——引擎全程接受目录参数，`tempfile::tempdir()` 即可全量测试。

**主要来源**：docs.rs/rig · rig.rs OpenAI 文档 · rig 根 Cargo.toml · MIGRATING.md · v0.44.0 release · OpenAI/Anthropic/DeepSeek docs.rs · Agent/AgentBuilder · rig-cassette · tests/README.md · docs.rs/rusqlite · rusqlite README · libsqlite3-sys · Rustfinity ORM 对比 · tauri PathResolver · Stack Overflow 77156078 · tauri#9936
