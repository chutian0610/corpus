import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vite'

// Tauri 集成要点（官方 Vite 指南）：
// - clearScreen 关闭，避免遮住 Rust 报错
// - 固定端口 + strictPort，被占用直接失败
// - 忽略 src-tauri 的文件变动
// - envPrefix 与 build.target 按 TAURI_ENV_* 适配
const host = process.env['TAURI_DEV_HOST']

// https://vite.dev/config/
export default defineConfig({
  plugins: [vue()],

  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: host || false,
    // 本地 web 真后端调试循环（票据 #17 v1）：`cargo run -p
    // corpus-server` + `pnpm dev`，/api（含 SSE）代理到本机服务——
    // 前端热更新 + 真引擎数据的开发闭环；浏览器从启动输出复制带
    // token 的 5173 地址打开（token 停在 sessionStorage，代理不参与鉴权）。
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:8619',
        // SSE 需要直通流式转发：http-proxy 默认不缓冲响应，无需额外配置
        ws: false,
      },
    },
    hmr: host
      ? {
          protocol: 'ws',
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  envPrefix: ['VITE_', 'TAURI_ENV_*'],
  build: {
    target:
      process.env['TAURI_ENV_PLATFORM'] === 'windows' ? 'chrome105' : 'safari13',
    // Vite 8（rolldown）下不再指定 'esbuild' 压缩器，用默认即可
    minify: process.env['TAURI_ENV_DEBUG'] ? false : true,
    sourcemap: !!process.env['TAURI_ENV_DEBUG'],
  },
})
