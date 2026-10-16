# Corpus 桌面应用

Tauri 2 + Vue 3 + Vite。领域逻辑在 workspace 的 `crates/corpus-engine`，这里是薄壳。

```bash
pnpm install
pnpm tauri dev   # 启动桌面应用（开发模式；debug 构建右键页面可「检查元素」）
pnpm dev         # 浏览器模式：用 Chrome 打开 http://localhost:5173
pnpm build       # 仅构建前端
```

## 浏览器模式（无 Tauri 壳的 UI 开发 / 调试）

`pnpm dev` 后直接用浏览器打开 http://localhost:5173：所有 Tauri command
（含目录选择对话框）被 `src/mocks/tauri-mocks.ts` 拦截为假实现，无需编译
Rust 即可开发与调试 UI（Chrome DevTools、自动化测试均可直接使用）。
mock 的响应字段形状与引擎 serde 输出一致（snake_case）；新增 command 时
需同步补 mock。Tauri 壳内此层自动跳过。
