import { createApp } from 'vue'
import './style.css'
import App from './App.vue'
import { isTauri } from './api'

/**
 * 壳与后端的接线顺序（双壳架构）：
 *
 * 1. **Tauri 壳内**（window.__TAURI_INTERNALS__ 存在）：直连真实后端
 *    （IPC invoke），与既往一致。
 * 2. **浏览器 + 生产构建**：由 corpus-server 静态服务本页（同源真
 *    后端），直接挂载。
 * 3. **浏览器 + dev**：探测 /api——server 在跑则直连真实后端
 *    （vite 代理转发，热更新 + 真数据调试闭环）；server 不在则落
 *    IPC mock（mocks/tauri-mocks.ts，纯 UI 离线开发不受影响）。
 *
 * 安全说明：web 形态无任何凭据——server 只绑 127.0.0.1 并校验
 * Host/Origin（本机源校验），跨源请求在服务端被拒。
 */

function mount() {
  createApp(App).mount('#app')
}

if (isTauri || !import.meta.env.DEV) {
  mount()
} else {
  // dev：探测真实后端（server 在跑则 vite 代理可达）
  const hasBackend = await fetch('/api/templates')
    .then((res) => res.ok)
    .catch(() => false)
  if (hasBackend) {
    mount()
  } else {
    console.info('Corpus：未检测到 corpus-server（/api 不可达），落 IPC mock（纯 UI 离线开发）。')
    import('./mocks/tauri-mocks').then(mount)
  }
}
