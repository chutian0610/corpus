# T1 脚手架前置调研笔记（Tauri 2 + Rust workspace + Vue 3）

调研日期：2026-10-15。目标：单一 cargo workspace 内放 corpus-engine（纯领域逻辑 crate）+ Tauri 2 桌面应用（Vue 3 + TS + Vite + pnpm）。
本机环境：macOS arm64、Rust 1.98、Node 22、pnpm 10。

## 0. 版本快照（事实）

- `tauri` crate 最新稳定版 **2.12.4**（2026-10-14 发布）；2.x 线活跃（2.12.3 于 10-10、2.12.2 于 10-09 发布）。3.0 仅有 alpha（3.0.0-alpha.5），不要用。
  来源：https://crates.io/api/v1/crates/tauri
- 依赖版本一律写 `"2"` / `{ version = "2" }` 走 2.x 最新补丁。

## 1. Workspace 布局（核心事实）

- 官方项目结构：JS 项目在顶层、Rust 项目嵌套在 `src-tauri/`。`tauri.conf.json` 是 Tauri CLI 定位 Rust 项目的**标记文件，必须与该 app 的 Cargo.toml 同目录**；`src-tauri/` 里还有 `build.rs`（跑 `tauri_build::build()`）、`capabilities/`、`icons/`、`src/lib.rs`（真正代码 + mobile entry point）与 `src/main.rs`（桌面入口，只调 `app_lib::run()`，官方说"不要改这个文件，改 lib.rs"；`app_lib` 对应 Cargo.toml 的 `[lib].name`）。
  来源：https://v2.tauri.app/start/project-structure
- 官方维护者（FabianLars）确认：一个 cargo workspace / monorepo 里放 Tauri app 完全可行，硬性要求就是 tauri.conf.json 与该 app 的 Cargo.toml 相邻；`src-tauri` 目录名可自定义；workspace 时 `target/` 与 Cargo.lock 落在**workspace 根**；Tauri CLI **没有** cargo `--manifest-path` 等价物，跑 `tauri dev/build` 要 cd 进 app 目录。
  来源：https://github.com/orgs/tauri-apps/discussions/13941
- 可行布局（推荐给 Corpus）：

```
corpus/
├── Cargo.toml              # [workspace]，members 含两个 crate
├── Cargo.lock              # 唯一，在根
├── target/                 # 在根
├── pnpm-workspace.yaml     # JS 侧 workspace（packages: [apps/desktop]）
├── crates/
│   └── corpus-engine/      # 纯领域逻辑：src/lib.rs、tests/
└── apps/
    └── desktop/            # 前端项目根（package.json、vite.config.ts、src/）
        └── src-tauri/      # Tauri Rust crate（Cargo.toml + tauri.conf.json 同目录）
```

### 根 Cargo.toml（可照抄）

```toml
[workspace]
resolver = "2"
members = [
    "crates/corpus-engine",
    "apps/desktop/src-tauri",
]

[workspace.package]
version = "0.1.0"
edition = "2021"

[workspace.dependencies]
corpus-engine = { path = "crates/corpus-engine" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
tempfile = "3"
tauri = { version = "2", features = [] }
tauri-build = { version = "2", features = [] }
tauri-plugin-dialog = "2"
```

成员 crate 里用 `tauri = { workspace = true }` 形式继承（`[workspace.dependencies]` + `workspace = true` 是 cargo 的 workspace 依赖继承机制）。参考：https://doc.rust-lang.org/cargo/reference/workspaces.html

### src-tauri/Cargo.toml（可照抄，CTA 模板同构）

```toml
[package]
name = "corpus-desktop"
version.workspace = true
edition.workspace = true

[lib]
name = "corpus_desktop_lib"
crate-type = ["staticlib", "cdylib", "rlib"]

[build-dependencies]
tauri-build = { workspace = true }

[dependencies]
tauri = { workspace = true }
tauri-plugin-dialog = { workspace = true }
corpus-engine = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
```

`build.rs` 一行：`fn main() { tauri_build::build() }`

`src/main.rs`（薄入口，不动它）：

```rust
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    corpus_desktop_lib::run()
}
```

## 2. 从零脚手架：两条路径

### 路径 A：create-tauri-app（CTA）

- 命令：`pnpm create tauri-app`（等价 `npm create tauri-app@latest` / `cargo install create-tauri-app --locked`）。交互式问：项目名、identifier（默认形如 com.tauri-app.app）、语言、包管理器、UI 模板、TS/JS flavor。
  来源：https://v2.tauri.app/start/create-project
- 模板 ID（README 明确列出）：`vanilla, vanilla-ts, vue, vue-ts, svelte, svelte-ts, react, react-ts, preact, preact-ts, solid, solid-ts, angular, yew, leptos, sycamore, blazor`，即 Vue+TS 用 `vue-ts`：`pnpm create tauri-app my-app --template vue-ts`（npm 7+ 需额外 `--`）。
  来源：https://github.com/tauri-apps/create-tauri-app
- 生成后：`pnpm install && pnpm tauri dev`。
- **局限**：CTA 生成的是"前端在仓库根 + src-tauri 在根"的单包布局。要进 workspace 需手工挪目录、把 src-tauri 挪到 apps/desktop/ 下、root 补 `[workspace]`、并修正 tauri.conf.json 里相对路径（`frontendDist`/`devUrl` 相对 src-tauri 所在位置解析——Vite 指南默认 `../dist`，嵌套后要相应调整）。

### 路径 B：手动搭（官方文档也写为 "manual" 流程）

1. 前端：在 `apps/desktop` 下 `pnpm create vite .`（选 vue-ts）。
2. `pnpm add -D @tauri-apps/cli@latest`（或 `cargo install tauri-cli --version "^2.0.0" --locked`）。
3. 记下 dev server 地址（Vite 默认 `http://localhost:5173`）。
4. `pnpm tauri init`：交互式问 app 名、窗口标题、web assets 路径（默认 `../dist`）、devUrl、dev/build 命令，生成 `src-tauri/`。
5. vite.config 里 ignore `src-tauri`。
6. `pnpm tauri dev` 验证。
来源：https://v2.tauri.app/start/create-project

### Vite 集成要点（官方 Vite 指南，可照抄进 tauri.conf.json 的 build 段）

```json
"build": {
  "beforeDevCommand": "pnpm dev",
  "beforeBuildCommand": "pnpm build",
  "devUrl": "http://localhost:5173",
  "frontendDist": "../dist"
}
```

`vite.config` 要点（官方原文）：`clearScreen: false`（别遮 Rust 报错）、`server.port: 5173` + `strictPort: true`（Tauri 要固定端口，被占直接失败）、`server.watch.ignored: ['**/src-tauri/**']`、`envPrefix: ['VITE_', 'TAURI_ENV_*']`、build.target 按 `TAURI_ENV_PLATFORM` 选 chrome105/safari13、debug 构建不 minify、出 sourcemap。
来源：https://v2.tauri.app/start/frontend/vite/

### 取舍结论（推荐路径）

**推荐路径 B（手动搭）+ 上面第 1 节的目录结构**：monorepo 双 crate 是硬需求，CTA 生成物反而要二次搬家改路径；手动路径每一步都可控且与官方 manual 流程一致。可折中：先在临时目录跑一次 CTA 参考，正式工程手动搭。

## 3. 前端调用 Rust（Tauri 2 语法）

来源：https://v2.tauri.app/develop/calling-rust

- 定义：`#[tauri::command] fn my_cmd(...) -> ...`。lib.rs 里的命令**不能是 pub**（胶水代码限制）；放独立模块的命令必须 `pub` 且名字跨模块唯一。
- 注册：`tauri::Builder::default().invoke_handler(tauri::generate_handler![cmd_a, cmd_b])`。**`invoke_handler` 只能调用一次，多次调用只有最后一次生效**；模块路径 `commands::my_cmd` 可用，JS 侧调用时忽略模块前缀。
- JS 侧（Tauri 2 的 import 路径是 `@tauri-apps/api/core`）：

```ts
import { invoke } from '@tauri-apps/api/core';
const msg = await invoke<string>('my_custom_command', { invokeMessage: 'Hello!' });
```

- 参数命名：JS 传 camelCase、Rust 收 snake_case（`invokeMessage` ↔ `invoke_message`）；要 JS 也用 snake_case 可 `#[tauri::command(rename_all = "snake_case")]`。参数类型需 `serde::Deserialize`，返回值需 `serde::Serialize`。
- **Result 错误映射**：命令返回 `Result<T, E>` 时，`Ok` → promise resolve，`Err` → promise reject（JS `try/catch` 或 `.catch` 捕获的是序列化后的 E）。**E 也必须实现 Serialize**，而大多数 std/第三方 error 没有：官方两个办法 `map_err(|e| e.to_string())`，或 thiserror 自定义错误 + 手写 `impl serde::Serialize`（可序列化成结构化对象映射 TS 联合类型）：

```rust
#[tauri::command]
async fn scan_corpus(root: String) -> Result<Vec<String>, String> {
    corpus_engine::scan(&root).map_err(|e| e.to_string())
}
```

- **异步命令**：官方明确"async 命令是首选"，`async fn` 命令经 `async_runtime::spawn` 在独立任务上执行，避免冻结 UI；非 async 命令在主线程跑（除非标 `#[tauri::command(async)]`）。JS 侧调用方式不变（invoke 本来就返回 promise）。
- 坑：async 命令**带引用参数（`&str`、`State<'_, T>`）时必须返回 `Result`**（见 issue #9797），最省事是直接用 owned 类型（`String`）。

## 4. tauri-plugin-dialog（文件夹选择）

来源：https://v2.tauri.app/plugin/dialog 、https://stackoverflow.com/questions/79153464/how-to-use-dialog-in-tauri-v2

- 一键安装：`cargo tauri add dialog`（或 `pnpm tauri add dialog`），自动完成 cargo 依赖 + builder 注册 + npm 包 + 权限。
- 手动三步：
  1. `cargo add tauri-plugin-dialog`（注意该插件**要求 Rust >= 1.90**，本机 1.98 满足）；
  2. Rust 注册：`tauri::Builder::default().plugin(tauri_plugin_dialog::init())`；
  3. `pnpm add @tauri-apps/plugin-dialog`。
- **capabilities 授权（必须，否则 JS 调用被拒）**：在 `src-tauri/capabilities/default.json` 的 permissions 数组加 `"dialog:default"`（默认集包含 allow-open/allow-save/allow-message）；只开选择对话框可用 `"dialog:allow-open"`。`allow-ask`/`allow-confirm` 已废弃（v3 将移除）。
- JS 选目录：

```ts
import { open } from '@tauri-apps/plugin-dialog';
const dir = await open({ directory: true, multiple: false }); // string | null（取消为 null）
```

（注：移动端不支持 folder picker，桌面无碍。）
- Rust 侧用法（可选）：`use tauri_plugin_dialog::DialogExt;` 后 `app.dialog().file().pick_folder(|path| ...)`（回调式，path 为 `Option`）或 `blocking_pick_folder()`；文档页展示的是 pick_file/blocking_pick_file 同族 API。

## 5. Rust 侧测试惯例

### tempfile

- **`tempdir` crate 已废弃**（RUSTSEC-2018-0017，2018 年起），官方推荐统一用 `tempfile`。
  来源：https://rustsec.org/advisories/RUSTSEC-2018-0017.html 、https://github.com/rust-lang-deprecated/tempdir/issues/45
- 推荐用法：`let dir = tempfile::tempdir()?;` 返回 `TempDir`（RAII guard，**drop 时自动删除**；不要丢弃返回值，否则目录立即被删）；要保留目录用 `.keep()` / `into_path()`。文件用 `tempfile::tempfile()` / `NamedTempFile`。
  来源：https://docs.rs/tempfile

### 测试放哪（Rust Book 第十一章惯例）

来源：https://doc.rust-lang.org/book/ch11-03-test-organization.html

- 单元测试：与被测代码**同文件**，`#[cfg(test)] mod tests { use super::*; ... }`，可测私有函数，只在 `cargo test` 时编译。
- 集成测试：crate 根的 `tests/` 目录，每个文件编译为**独立 crate**，只能调公共 API，不需要 `#[cfg(test)]`；共享助手放 `tests/common/mod.rs`（避免 common.rs 被当成测试文件）。
- 只有 main.rs 没有 lib.rs 的二进制 crate 写不了集成测试 → 逻辑进 lib.rs。src-tauri 模板"main.rs 薄 + lib.rs 逻辑"的形态天然满足。
- workspace 下：根目录 `cargo test` 跑全部成员；`cargo test -p corpus-engine` 只跑引擎。Engine 的测试策略：`#[cfg(test)]` 单元测试为主 + `tests/` 放涉及真实文件系统的集成场景（用 tempfile 造临时语料目录）。workspace 惯例参考：https://doc.rust-lang.org/cargo/reference/workspaces.html

## 6. tauri dev / tauri build 的 macOS 本地要求（仅本机运行，不签名分发）

- **桌面开发只需要 Xcode Command Line Tools**：`xcode-select --install`；完整 Xcode 仅 iOS 需要（官方 prerequisites + 社区确认）。
  来源：https://v2.tauri.app/start/prerequisites 、https://github.com/pytauri/pytauri/discussions/260
- 其余即本机已有：Rust（dialog 插件要求 ≥1.90）、Node 22、pnpm 10。
- `tauri dev`：无需任何签名配置。首次全量编译依赖较多，耗时数分钟属正常。
- `tauri build`：产物 `.app`（可 `--bundles dmg` 出磁盘镜像）；不配置签名身份也能构建，无 Apple 开发者账号可本地使用。Apple Silicon 上如遇运行问题可显式 ad-hoc 签名（`signingIdentity: "-"`）；公证（notarization）只影响外部分发，本机场景跳过。
  来源：https://v2.tauri.app/distribute/sign/macos/

## 7. 已知版本坑清单

1. **identifier 规则**：只允许 `[A-Za-z0-9-.]`，**下划线不允许**（常见报错源）；必须改掉默认 `com.tauri.dev`；用反向域名（如 `dev.corpus.desktop`），发布后不可再改。
   来源：https://v2.tauri.app/reference/config 、https://github.com/tauri-apps/tauri/issues/9707
2. **capabilities 机制**：capabilities 目录下文件默认全部启用，但**一旦在 tauri.conf.json 显式列出 capabilities，就只用列出的**；`windows` 字段要匹配窗口 label（默认 `main`）；插件权限格式 `<plugin>:<permission>`（如 `dialog:default`）；`$schema` 指向 `../gen/schemas/desktop-schema.json` 获得 IDE 补全。自己注册的 `#[tauri::command]` 默认全窗口可用，不受此限。
   来源：https://v2.tauri.app/security/capabilities/
3. **pnpm 10 默认拦截依赖构建脚本**（esbuild 的 postinstall 等），装完依赖跑不起来时执行 `pnpm approve-builds`（写入 pnpm-workspace.yaml 的 onlyBuiltDependencies/allowBuilds）后重装；`pnpm ignored-builds` 可查被拦的包。
   来源：https://pnpm.io/cli/approve-builds 、https://pnpm.io/settings/build
4. **Tauri CLI 无 `--manifest-path`**：workspace 里必须在 app 目录（或其 package.json scripts）里跑 `tauri dev/build`，根目录直接跑找不到配置。
   来源：https://github.com/orgs/tauri-apps/discussions/13941
5. **`invoke_handler` 只能挂一次**，链式写两次后者覆盖前者（把命令集中到一个 `generate_handler!` 列表）。
   来源：https://v2.tauri.app/develop/calling-rust
6. **async 命令 + 引用参数必须返回 Result**；否则用 owned 参数。
   来源：https://github.com/tauri-apps/tauri/issues/9797
7. **CLI 与 crate 大版本要配对**：统一用 `^2`（`cargo install tauri-cli --version "^2.0.0" --locked` 或 pnpm 的 `@tauri-apps/cli@latest`）。
   来源：https://v2.tauri.app/start/create-project
8. **路径相对性**：`frontendDist`（默认 `../dist`）与 `devUrl` 相对 src-tauri 位置解析；src-tauri 嵌套到 `apps/desktop/` 下后需按新层级核对（前端若在 apps/desktop 根，`../dist` 仍正确——相对的是 src-tauri 上一级）。
   来源：https://v2.tauri.app/start/frontend/vite/

## 8. 可照抄：tauri.conf.json 与 capabilities

`apps/desktop/src-tauri/tauri.conf.json`：

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Corpus",
  "version": "0.1.0",
  "identifier": "dev.corpus.desktop",
  "build": {
    "beforeDevCommand": "pnpm dev",
    "beforeBuildCommand": "pnpm build",
    "devUrl": "http://localhost:5173",
    "frontendDist": "../dist"
  },
  "app": {
    "windows": [
      { "label": "main", "title": "Corpus", "width": 1200, "height": 800 }
    ],
    "security": { "csp": null }
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "icon": [
      "icons/32x32.png", "icons/128x128.png", "icons/128x128@2x.png",
      "icons/icon.icns", "icons/icon.ico"
    ]
  }
}
```

`apps/desktop/src-tauri/capabilities/default.json`：

```json
{
  "$schema": "../gen/schemas/desktop-schema.json",
  "identifier": "default",
  "description": "Corpus 主窗口能力：核心默认 + 对话框",
  "windows": ["main"],
  "permissions": ["core:default", "dialog:default"]
}
```

`apps/desktop/src-tauri/src/lib.rs`：

```rust
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![scan_corpus])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[tauri::command]
async fn scan_corpus(root: String) -> Result<Vec<String>, String> {
    corpus_engine::scan(&root).map_err(|e| e.to_string())
}
```

前端调用（`apps/desktop/src/`）：

```ts
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';

const dir = await open({ directory: true, multiple: false });
if (typeof dir === 'string') {
  const files = await invoke<string[]>('scan_corpus', { root: dir });
}
```

## 9. 推荐路径结论

1. 手动脚手架（路径 B）：根 workspace + `crates/corpus-engine` + `apps/desktop`（Vite vue-ts + src-tauri），不用 CTA 生成再搬家。
2. 根 `Cargo.toml` 用 `[workspace.dependencies]` 统一版本，`tauri = "2"`（当前 2.12.4）、`tauri-plugin-dialog = "2"`、`tempfile = "3"`（弃用 tempdir）。
3. engine 测试：crate 内 `#[cfg(test)]` 单元 + `tests/` 集成（tempfile 造目录）；src-tauri 保持 main.rs 薄、逻辑走 lib.rs 并委托 corpus-engine。
4. 命令层统一 `async fn` + `Result<T, String>`（后续换 thiserror 结构化错误），JS 用 `@tauri-apps/api/core` 的 invoke。
5. dialog 三件套（cargo 依赖 + `.plugin(init())` + npm 包）加 `dialog:default` 权限；identifier 定 `dev.corpus.desktop`（无下划线）。
6. macOS 本机：CLT 已够，dev/build 均无需签名；pnpm 10 记得 approve-builds。
