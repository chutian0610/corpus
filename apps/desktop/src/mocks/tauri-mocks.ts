/**
 * 浏览器模式的 IPC mock 层（仅开发期加载，见 main.ts 的接线）。
 *
 * `pnpm dev` 后用浏览器直接打开 http://localhost:5173 即可在无 Tauri 壳的
 * 环境里开发 / 调试 UI：所有 Tauri command 与 dialog 插件调用都被拦截为
 * 下面的假实现，字段形状与引擎 serde 输出保持一致（snake_case）。
 *
 * 模板元数据镜像自 crates/corpus-engine/src/template.rs 的编译期常量
 * （同 TEMPLATE_DISPLAY_NAMES 的展示层镜像模式）。
 */
import { mockIPC } from '@tauri-apps/api/mocks'
import type { InvokeArgs } from '@tauri-apps/api/core'

interface TemplateInfo {
  id: string
  name: string
  description: string
}

const TEMPLATES: TemplateInfo[] = [
  { id: 'generic', name: 'Generic', description: '通用知识库：以 Entity / Concept / Topic 组织日常积累，适合大多数场景。' },
  { id: 'research', name: 'Research', description: '研究：围绕课题的问题、方法与文献脉络组织页面，便于追源与对比。' },
  { id: 'reading', name: 'Reading', description: '阅读：以书摘、作者、人物与主题沉淀长期阅读，构建个人阅读图谱。' },
]

const DEFAULT_CONFIG = {
  fast: { provider: 'openai', model: '', base_url: null, api_key_env: null },
  strong: { provider: 'openai', model: '', base_url: null, api_key_env: null },
  apply_mode: 'manual',
  query_pages: true,
}

/** 最近 Vault 的内存态：让设置页的打开 / 移除交互在浏览器里真实可玩。 */
let recentVaults = [
  { path: '/tmp/demo-vault', template_id: 'research', opened_at: new Date().toISOString() },
]

function upsertRecent(path: string, templateId: string): void {
  const now = new Date().toISOString()
  recentVaults = [
    { path, template_id: templateId, opened_at: now },
    ...recentVaults.filter((v) => v.path !== path),
  ]
}

mockIPC((cmd, payload) => {
  const args = (payload ?? {}) as InvokeArgs
  switch (cmd) {
    case 'list_templates':
      return TEMPLATES
    case 'create_vault': {
      const { path, templateId } = args as { path: string; templateId: string }
      const template = TEMPLATES.find((t) => t.id === templateId)
      if (!template) {
        throw `未知模板 id：${templateId}（可用模板：${TEMPLATES.map((t) => t.id).join(', ')}）`
      }
      upsertRecent(path, templateId)
      return {
        root: path,
        template_id: templateId,
        template_name: template.name,
        created_paths: [
          'raw/',
          'raw/.gitkeep',
          'wiki/',
          'wiki/index.md',
          'wiki/log.md',
          'purpose.md',
          '.corpus/',
          '.corpus/.gitkeep',
        ],
      }
    }
    case 'get_config':
      return structuredClone(DEFAULT_CONFIG)
    case 'set_config':
      return (args as { config: unknown }).config
    case 'test_connection':
      return { latency_ms: 120, reply: 'OK（浏览器模式假实现）' }
    case 'list_recent_vaults':
      return recentVaults
    case 'add_recent_vault': {
      const { path, templateId } = args as { path: string; templateId: string }
      upsertRecent(path, templateId)
      return null
    }
    case 'remove_recent_vault': {
      const { path } = args as { path: string }
      recentVaults = recentVaults.filter((v) => v.path !== path)
      return null
    }
    // 目录选择对话框：返回演示路径，让「浏览…」按钮在浏览器里可用
    case 'plugin:dialog|open':
      return '/tmp/demo-vault'
    default:
      throw `浏览器模式未实现该命令的 mock：${cmd}`
  }
})
