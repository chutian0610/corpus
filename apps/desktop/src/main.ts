import { createApp } from 'vue'
import './style.css'
import App from './App.vue'
import { CORPUS_TOKEN_KEY, isTauri } from './api'

/**
 * 壳与后端的接线顺序（票据 #17 v1，双壳架构）：
 *
 * 1. **Tauri 壳内**（window.__TAURI_INTERNALS__ 存在）：直连真实后端
 *    （IPC invoke），与既往一致。
 * 2. **浏览器 + 有真实后端可连**（URL 带 ?token= 或 sessionStorage 已
 *    有令牌——即从 corpus-server 启动输出的 URL 打开，或同一会话内已
 *    引导过）：直连 corpus-server（fetch /api + SSE，token 见 api.ts）。
 * 3. **浏览器 + dev + 无后端可连**：落 IPC mock（mocks/tauri-mocks.ts
 *    保留，纯 UI 离线开发不受影响）。
 * 4. **浏览器 + 生产构建 + 无令牌**：仍直连挂载——corpus-server 静态
 *    服务的页面本该带 token 打开；令牌缺失时 api.ts 会在首次请求 401
 *    时 console 给出明确指路（生产构建没有 mock 可落）。
 *
 * token 引导：?token= 一次性写入 sessionStorage 后用
 * history.replaceState 从地址栏清除——token 不留在历史 / 分享链接里，
 * 后续请求只走 X-Corpus-Token 头。isTauri 与 token 键均复用 api.ts 的
 * 模块级单次判定（不在本文件重复声明）。
 */
const urlToken = new URLSearchParams(window.location.search).get('token')
if (urlToken) {
  sessionStorage.setItem(CORPUS_TOKEN_KEY, urlToken)
  window.history.replaceState(null, '', window.location.pathname)
}

function mount() {
  createApp(App).mount('#app')
}

const hasToken = Boolean(urlToken) || Boolean(sessionStorage.getItem(CORPUS_TOKEN_KEY))

if (isTauri) {
  mount()
} else if (import.meta.env.DEV && !hasToken) {
  // 浏览器模式离线开发：先装 IPC mock 再挂载，保证首个 onMounted 的
  // invoke 即被拦截。生产构建 DEV 为假，整段不生效。
  import('./mocks/tauri-mocks').then(mount)
} else {
  mount()
}
