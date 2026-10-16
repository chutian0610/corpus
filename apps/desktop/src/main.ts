import { createApp } from 'vue'
import './style.css'
import App from './App.vue'

// 浏览器模式（pnpm dev 后用浏览器打开，无 Tauri 壳）：
// 先装 IPC mock，再挂载应用，保证首个 onMounted 的 invoke 即被拦截。
// Tauri 壳内 window.__TAURI_INTERNALS__ 存在，跳过；生产构建 DEV 为假，整段不生效。
if (import.meta.env.DEV && typeof (window as any).__TAURI_INTERNALS__ === 'undefined') {
  import('./mocks/tauri-mocks').then(() => createApp(App).mount('#app'))
} else {
  createApp(App).mount('#app')
}
