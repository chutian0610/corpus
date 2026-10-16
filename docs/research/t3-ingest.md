# T3 Ingest 前置调研笔记（核实日期：2026-10-16）

> 范围：corpus-engine（Rust，UI 无关）Ingest 管道所需的事实核实——frontmatter YAML、wikilink、原子写、Tauri 2 进度事件、rig 0.44 结构化输出、SHA-256。
> 本地现状（已核对根 Cargo.toml 与 crates/corpus-engine/Cargo.toml）：workspace 已有 `rig = "=0.44.0"`、`tempfile = "3"`（仅 dev-dep）、`serde`/`serde_json`/`chrono`/`thiserror`、`tauri = "2"`；**尚无** sha2 / schemars / yaml / markdown 相关依赖。

## 0. 结论速览（建议新增依赖）

| 用途 | 推荐 | 版本 | 说明 |
|---|---|---|---|
| YAML frontmatter 解析+序列化 | `serde_yaml_ng` | 0.10 | serde_yaml 的维护 fork，drop-in，双向 |
| Markdown/wikilink 解析（校验用） | `pulldown-cmark` | 0.13 | 原生 `ENABLE_WIKILINKS`（Obsidian 风格） |
| frontmatter+正文分离（读现成页面时） | `gray_matter`（可选） | 0.3 | 仅解析；生成侧不需要它 |
| 结构化输出 schema derive | `schemars` | **1.x**（当前 1.2.3） | 仅在用 rig 原生结构化输出时需要；rig-core 0.44 要求 `schemars ^1`，**不能**用 0.8 |
| SHA-256 | `sha2` | 0.10（0.10.8） | 与 rig-core 0.44 内部依赖一致，避免双版本 |
| 原子写 | `tempfile`（已有） | 3（当前 3.27.0） | 从 dev-dep 提升为正式依赖 |

生成端（写 Markdown 页面）不需要 Markdown 解析器：frontmatter 用 `serde_yaml_ng::to_string` 拼接，正文含 `[[...]]` 直接写字符串即可。

## 1. YAML frontmatter crate 现状（2026-10）

### 事实

- **`serde_yaml` 已死**：dtolnay 于 2024-03 归档仓库并发布最终版 `0.9.34+deprecated`，明确不再维护。
- **`serde_yml` 避开**：该"接棒者"本身也已停维护、页面已转为指向迁移说明（社区对其代码质量有大量争议，serde_yaml_ng README 直接点名批评）。不要选。
- **候选**（crates.io API 核实的版本/日期）：
  - `serde_yaml_ng`：最新 **0.10.0**（2024-05-26），repo `acatton/serde-yaml-ng`，YAML 1.1，serde_yaml 的"最小改动维护 fork"，API drop-in。
  - `serde_norway`：最新 0.9.42（2024-12-21），同为 serde_yaml 系 fork。
  - `yaml-rust2`：最新 **0.13.0**（2026-09-11，活跃），但是纯解析事件流，**无 serde 序列化**——不适合"双向"。
- **`gray_matter`**（frontmatter+正文分离专用）：最新 **0.3.2**（2025-07-10），支持 YAML/TOML/JSON/自定义 `Engine` trait；**只解析，不负责生成 frontmatter**。

### 推荐

**`serde_yaml_ng = "0.10"`**，一个 crate 同时覆盖 Ingest 的读与写。若未来停更，切换到 `serde_norway` 基本是改名级成本。

frontmatter 的分离与拼接逻辑极简，**手写**即可（生成端必须手写，因为要精确控制 `---` 与正文之间的空行）；读取端若要处理 excerpt、多种 delimiter 等边角情况再引入 `gray_matter`。

### 可照抄片段

```rust
// Cargo.toml: serde_yaml_ng = "0.10"
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct PageFrontmatter {
    pub title: String,
    pub fingerprint: String,      // SHA-256 hex
    pub source: String,           // 原始文件相对路径
    pub entities: Vec<String>,    // 正文中将生成 [[entity]] 的目标
    pub created: String,          // RFC 3339
}

// 生成：serde 按结构体字段声明顺序输出 YAML（serde 保序），尾部自带一个换行
pub fn render_page(fm: &PageFrontmatter, body: &str) -> String {
    let yaml = serde_yaml_ng::to_string(fm).expect("frontmatter serialize");
    format!("---\n{}---\n\n{}\n", yaml, body)
}

// 解析：手写分离（生成端格式是自己定的，够用）
pub fn split_frontmatter(src: &str) -> Option<(String, String)> {
    let rest = src.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    Some((rest[..end].to_string(), rest[end + 4..].trim_start_matches('\n').to_string()))
}
```

陷阱提醒：
- Obsidian 对 frontmatter 里列表/多行字符串都兼容，`to_string` 对非 ASCII 默认不转义（YAML UTF-8 合法，Obsidian 正常读）。
- 不要用 `serde_yaml_ng` 解析不可信的任意 YAML（YAML 1.1 语义坑多）；Ingest 只回读自己生成的文件，风险可控。

## 2. Wikilink 处理（`[[target]]` / `[[target|alias]]`）

### 事实

- **`pulldown-cmark` 当前最新 0.13.4**（2026-05-20）。
- **0.13.0 新增 `Options::ENABLE_WIKILINKS`**，官方文档描述即 "**Obsidian-style Wikilinks**"（PR #991）。这是原生扩展，不再是社区自造。
- **事件形态**：wikilink 不是新 Tag，而是走既有 `Tag::Link`，`LinkType` 新增变体 **`LinkType::WikiLink { has_pothole: bool }`**（`has_pothole = true` 表示带管道别名 `[[foo|bar]]`）。`dest_url` 是目标，别名是 `Start/End` 之间的内联事件（可含格式）。
- PR #991 测试确认支持：`[[target]]`、`[[target|alias]]`、嵌套路径 `[[Nested/WikiLink]]`、锚点 `[[target#heading]]`、嵌入 `![[dog.png|100]]`（走 image embed 路径）。

### 推荐

- 生成端：直接字符串拼接 `format!("[[{}]]", entity)`，别名用 `[[target|alias]]`。
- 解析端（回读校验链接完整性）：`pulldown-cmark = "0.13"` + `ENABLE_WIKILINKS`，遍历事件收集 `LinkType::WikiLink`。不做 regex 预扫描。

```rust
// Cargo.toml: pulldown-cmark = "0.13"
use pulldown_cmark::{Event, Options, Parser, Tag, LinkType};

pub fn extract_wikilinks(md: &str) -> Vec<String> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_WIKILINKS);
    let mut out = Vec::new();
    for ev in Parser::new_ext(md, opts) {
        if let Event::Start(Tag::Link { link_type: LinkType::WikiLink { .. }, dest_url, .. }) = ev {
            out.push(dest_url.to_string()); // [[target|alias]] 的 dest_url == "target"
        }
    }
    out
}
```

## 3. 原子写文件（tempfile）

### 事实

- `tempfile` 当前版本 **3.27.0**。corpus 已用 `tempfile = "3"`（目前只在 dev-deps，需提升到 `[dependencies]`）。
- **惯用法**：`NamedTempFile::new_in(target_parent)`（关键：与目标同目录 → 同文件系统）→ `write_all` →（可选 `sync_all`）→ `persist(path)` = **原子 rename，已存在则原子替换**。
- **陷阱**（docs.rs 官方文档确认）：
  - "Temporary files **cannot be persisted across filesystems**"——跨设备 rename 失败（EXDEV）；必须 `new_in(目标父目录)`，别用系统 /tmp。
  - `persist` 返回 `PersistError { file, error }`，`error.file` 可以拿回原 `NamedTempFile` 重试，不要丢弃。
  - `persist_noclobber` "not guaranteed to be atomic on all platforms"——覆盖写 index.md/log.md 用 `persist`，不用 noclobber。
  - durability：`persist` 返回后不保证已同步到磁盘；commit 前防掉电在 persist 前 `file.sync_all()`。
  - Windows：`persist` 是原子替换（`MoveFileEx`+replace）；目标被其他进程打开时 rename 失败属正常 IO 错误路径。

### 可照抄片段

```rust
use std::io::Write;
use std::path::Path;
use tempfile::NamedTempFile;

pub fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().expect("target must have parent");
    let mut tmp = NamedTempFile::new_in(dir)?;   // 同目录 => 同文件系统，避免 EXDEV
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;                    // 可选：commit 前保证数据落盘
    tmp.persist(path)
        .map_err(|e| std::io::Error::new(e.error.kind(), format!("persist {}: {e}", path.display())))?;
    Ok(())
}
```

## 4. Tauri 2：命令执行中向前端推进度

### 事实（官方 v2 文档，页面 2026-09 更新）

- `AppHandle` 与 `WebviewWindow` 实现了 `Emitter`/`Listener` trait。Rust 侧 `use tauri::{AppHandle, Emitter};` 后即可 `app.emit(event, payload)`；payload 要求 `Serialize + Clone`。
- 前端 `import { listen } from '@tauri-apps/api/event'`，`listen` 返回 `Promise<UnlistenFn>`，**必须 `await` 后在卸载时调用 `unlisten()`**；SPA 路由切换不会自动注销，需手动清理。
- 通道区别：`emit`（全局广播）、`emit_to`、`emit_filter`、**`Channel`**（command 参数级点对点，"fast and deliver ordered data"，适合高频有序流）。事件系统定位小数据量通信——Ingest 三阶段是低频粗粒度事件，用 `emit` 足够；未来 token 级流式再换 `Channel`。
- 长任务惯用法：**async command** + 命令内循环 `app.emit(...)` 推进度。

### 可照抄片段

```rust
// src-tauri（Tauri 2）
use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Clone, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum IngestProgress {
    Reading { path: String },
    Analyzing { entities: usize },
    Writing { page: String },
    Done { page: String, commit: String },
}

#[tauri::command]
async fn ingest(app: AppHandle, path: String) -> Result<String, String> {
    let _ = app.emit("ingest-progress", IngestProgress::Reading { path: path.clone() });
    // ... corpus-engine 调用 ...
    Ok("ok".into())
}
```

```ts
// Vue 3 前端
import { onMounted, onUnmounted, ref } from 'vue';
import { listen } from '@tauri-apps/api/event';

const stage = ref('idle');
let unlisten: (() => void) | null = null;

onMounted(async () => {
  unlisten = await listen('ingest-progress', (e) => { stage.value = e.payload.stage; });
});
onUnmounted(() => unlisten?.());
```

关键架构提示：corpus-engine 不应依赖 tauri——engine 暴露带回调的 `ingest(..., on_progress: impl FnMut(Stage))`，Tauri 壳层把回调转成 `app.emit`。

## 5. rig 0.44 结构化输出（第一步"结构化分析"）

### 事实（docs.rs rig 0.44.0 + rig.rs 官方文档核实）

**有一等支持**：`AgentBuilder::output_schema::<T>()`（约束为 T 的 JSON schema）、`output_schema_raw(schema)`、`output_mode(mode)`；取回用 `agent.prompt_typed::<T>(...).retries(n).await?.output` → 强类型 T，或 `ExtractorBuilder`。错误类型 `StructuredOutputError`（EmptyResponse / Deserialization{output,error} / Prompt）。官方提示：Tool 与 Prompted 模式是 best-effort，调用者需校验。

- **版本耦合（关键）**：rig-core 0.44.0 的必需依赖含 `schemars ^1`。用户侧 `#[derive(JsonSchema)]` 必须用 **schemars 1.x**——0.8 的 trait 与 1.x 不兼容。

### 架构建议（对本项目）

`output_schema`/`prompt_typed` 属于 rig 的 agent 层 API；corpus 已有 `LlmPort` 端口。**T3 的测试基线是 scripted fake + 引擎侧 JSON 解析容错（prompt 要求 JSON + serde_json 解析 + 一次重试），LlmPort 保持 `complete(system, user)` 单方法、provider 无关**；rig 原生 output_schema/prompt_typed（需 schemars 1.x）可在 RigLlmPort adapter 内做增强，属后续优化，不是 T3 验收必需。这样 fake 测试确定性、`--no-default-features` 路径不变。

```rust
// 若走 rig 原生增强（后续优化）：
// Cargo.toml: schemars = "1"  // 必须 1.x，与 rig-core 0.44 对齐
let agent = AgentBuilder::new(model)
    .preamble("你是知识库分析器……")
    .output_schema::<Analysis>()
    .build();
let resp = agent.prompt_typed::<Analysis>(&prompt_text).retries(2).await?;
let analysis: Analysis = resp.output;
```

## 6. SHA-256：sha2 crate 版本

- `sha2` 最新 **0.11.0**（2026-03-25，edition 2024、SHA-NI 硬件加速）；0.10 系列最后一版 **0.10.8**，生态存量巨大。
- **rig-core 0.44.0 内部依赖 `sha2 ^0.10`**。

**推荐 `sha2 = "0.10"`**：输出与 0.11 完全一致；与 rig 共用同一份编译，避免双版本。文档级指纹用不到硬件加速。

```rust
// Cargo.toml: sha2 = "0.10"
use sha2::{Digest, Sha256};

pub fn fingerprint(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}
```

## 附：落地改动清单

```toml
# 根 Cargo.toml [workspace.dependencies] 新增：
serde_yaml_ng = "0.10"
pulldown-cmark = "0.13"     # 回读校验用
sha2 = "0.10"
# tempfile = "3" 已存在；把 corpus-engine 的 tempfile 从 dev-dependencies 提到 dependencies
# schemars = "1"             # 仅在启用 rig 原生结构化输出增强时加
```

版本速查（2026-10-16 经 crates.io API / docs.rs 核实）：serde_yaml_ng 0.10.0 · pulldown-cmark 0.13.4 · gray_matter 0.3.2 · tempfile 3.27.0 · schemars 1.2.3 · sha2 0.10.8 / 0.11.0 · rig-core 0.44.0（依赖 schemars ^1、sha2 ^0.10）。
