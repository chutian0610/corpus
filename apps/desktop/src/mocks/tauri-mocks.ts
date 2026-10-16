/**
 * 浏览器模式的 IPC mock 层（仅开发期加载，见 main.ts 的接线）。
 *
 * `pnpm dev` 后用浏览器直接打开 http://localhost:5173 即可在无 Tauri 壳的
 * 环境里开发 / 调试 UI：所有 Tauri command 与 dialog 插件调用都被拦截为
 * 下面的假实现，字段形状与引擎 serde 输出保持一致（snake_case）。
 *
 * 模板元数据镜像自 crates/corpus-engine/src/template.rs 的编译期常量
 * （同 TEMPLATE_DISPLAY_NAMES 的展示层镜像模式）。
 *
 * T-UX 同步：open_vault（假校验——只认「已知 Vault」集合，非 Vault 目录
 * 报含路径的中文错误）、list_pages / read_page（假页面树，摄取成功后
 * 会长出一页 source）、create_vault 对已知 Vault 报「请从启动页打开」。
 *
 * 事件：mockIPC 开启 shouldMockEvents 后，前端 `emit` 会触达 `listen`
 * 注册的回调——ingest_file 的假进度借此走真实事件路径（不落盘）。
 */
import { mockIPC } from '@tauri-apps/api/mocks'
import type { InvokeArgs } from '@tauri-apps/api/core'

interface TemplateInfo {
  id: string
  name: string
  description: string
}

/** IngestReport / IngestProgress / PageTree 的形状镜像自引擎 serde 输出。 */
interface IngestedPage {
  path: string
  title: string
  page_type: 'source' | 'entity' | 'concept'
  action: 'create' | 'update'
}

interface IngestReport {
  skipped: boolean
  applied: boolean
  fingerprint: string
  source_title: string
  pages: IngestedPage[]
  commit: string | null
  summary_lines: string[]
  note: string | null
}

type IngestProgress =
  | { stage: 'reading'; raw: string }
  | { stage: 'analyzing' }
  | { stage: 'generating'; pages: number }
  | { stage: 'applying'; pages: number }

interface PageEntry {
  path: string
  updated: string | null
}

interface PageGroup {
  kind: 'source' | 'entity' | 'concept'
  pages: PageEntry[]
}

interface PageTree {
  groups: PageGroup[]
}

/** 浏览器模式假摄取的产出（不落盘，仅供 UI 调试）。 */
const FAKE_INGEST_PAGES: IngestedPage[] = [
  { path: 'sources/attention-is-all-you-need.md', title: 'Attention Is All You Need', page_type: 'source', action: 'create' },
  { path: 'entities/transformer.md', title: 'Transformer', page_type: 'entity', action: 'create' },
  { path: 'concepts/自注意力.md', title: '自注意力', page_type: 'concept', action: 'create' },
]

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

/** 最近 Vault 的内存态：启动页的打开 / 移除交互在浏览器里真实可玩。 */
let recentVaults = [
  { path: '/tmp/demo-vault', template_id: 'research', opened_at: new Date().toISOString() },
]

/** 「已知 Vault」集合：open_vault 假校验的唯一事实源。
 * 种子即演示 Vault（目录对话框的固定返回值，因此「打开其他 Vault」
 * 永远成功、对它新建则演示「请从启动页打开」的报错分支）。 */
const knownVaults = new Map<string, string>([['/tmp/demo-vault', 'research']])

function upsertRecent(path: string, templateId: string): void {
  const now = new Date().toISOString()
  recentVaults = [
    { path, template_id: templateId, opened_at: now },
    ...recentVaults.filter((v) => v.path !== path),
  ]
}

/** 假页面树：基础三页 + 假摄取（auto 模式）成功后追加的 source 页。 */
let extraSourcePages: PageEntry[] = []

function fakePageTree(): PageTree {
  const updated = '2026-10-16T10:30:00+08:00'
  return {
    groups: [
      {
        kind: 'source',
        pages: [{ path: 'sources/attention-is-all-you-need.md', updated }, ...extraSourcePages],
      },
      { kind: 'entity', pages: [{ path: 'entities/transformer.md', updated }] },
      { kind: 'concept', pages: [{ path: 'concepts/自注意力.md', updated }] },
    ],
  }
}

/** 假页面原文（read_page）：与假页面树一一对应的 frontmatter + 正文。 */
function fakePageContent(relPath: string): string {
  const titles: Record<string, string> = {
    'sources/attention-is-all-you-need.md': 'Attention Is All You Need',
    'entities/transformer.md': 'Transformer',
    'concepts/自注意力.md': '自注意力',
  }
  const title = titles[relPath] ?? relPath
  return [
    '---',
    `type: ${relPath.startsWith('sources/') ? 'source' : relPath.startsWith('entities/') ? 'entity' : 'concept'}`,
    `title: ${title}`,
    `aliases: [${title}]`,
    'tags: []',
    'related: []',
    'sources: []',
    'created: 2026-10-16T10:30:00+08:00',
    'updated: 2026-10-16T10:30:00+08:00',
    '---',
    '',
    `# ${title}`,
    '',
    '（浏览器模式假页面内容——正式渲染属 T4。）',
    '相关页面：[[entities/transformer|Transformer]]、[[concepts/自注意力]]。',
    '',
  ].join('\n')
}

/**
 * 假摄取（模拟默认配置 apply_mode=manual）：按 400ms 间隔推进进度事件
 * （走 mock 事件通道；人审模式无落盘阶段），最后 resolve 未落盘的
 * Draft 检视清单。不写任何文件；apply_mode=auto 时模拟落盘成功并
 * 让页面树长出一页 source。
 */
async function fakeIngest(rawPath: string, applyMode: string): Promise<IngestReport> {
  const { emit } = await import('@tauri-apps/api/event')
  const script: IngestProgress[] = [
    { stage: 'reading', raw: rawPath },
    { stage: 'analyzing' },
    { stage: 'generating', pages: FAKE_INGEST_PAGES.length },
  ]
  for (const step of script) {
    await new Promise((resolve) => setTimeout(resolve, 400))
    await emit('ingest-progress', step)
  }
  if (applyMode === 'auto') {
    await new Promise((resolve) => setTimeout(resolve, 400))
    await emit('ingest-progress', { stage: 'applying', pages: FAKE_INGEST_PAGES.length })
    // 模拟落盘：页面树长出一页 source（真实引擎由文件系统驱动）
    const stamp = new Date().toISOString()
    extraSourcePages = [
      ...extraSourcePages,
      { path: `sources/${rawPath.split('/').pop()?.replace(/\.(md|txt)$/, '') ?? 'browser-mock'}.md`, updated: stamp },
    ]
    return {
      skipped: false,
      applied: true,
      fingerprint: 'browser-mock-fingerprint',
      source_title: 'Attention Is All You Need',
      pages: FAKE_INGEST_PAGES,
      commit: 'abc1234',
      summary_lines: [],
      note: null,
    }
  }
  return {
    skipped: false,
    applied: false,
    fingerprint: 'browser-mock-fingerprint',
    source_title: 'Attention Is All You Need',
    pages: FAKE_INGEST_PAGES,
    commit: null,
    summary_lines: [
      'source: Attention Is All You Need (raw: raw/paper.md, sha256: browser-mock-fingerprint)',
      'Create sources/attention-is-all-you-need.md: Attention Is All You Need',
      'Create entities/transformer.md: Transformer',
      'Create concepts/自注意力.md: 自注意力',
    ],
    note: '人审模式：Draft 未落盘，完整人审流程属 T5',
  }
}

mockIPC(
  (cmd, payload) => {
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
        if (knownVaults.has(path)) {
          throw `该目录已是一个 Vault，请从启动页打开：${path}`
        }
        knownVaults.set(path, templateId)
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
            '.corpus/.gitignore',
            '.corpus/vault.json',
          ],
        }
      }
      case 'get_config':
        return structuredClone(DEFAULT_CONFIG)
      case 'set_config': {
        const config = (args as { config: typeof DEFAULT_CONFIG }).config
        DEFAULT_CONFIG.apply_mode = config.apply_mode
        DEFAULT_CONFIG.query_pages = config.query_pages
        return structuredClone(config)
      }
      case 'test_connection':
        return { latency_ms: 120, reply: 'OK（浏览器模式假实现）' }
      case 'list_recent_vaults':
        return recentVaults
      case 'remove_recent_vault': {
        const { path } = args as { path: string }
        recentVaults = recentVaults.filter((v) => v.path !== path)
        return null
      }
      case 'open_vault': {
        const { path } = args as { path: string }
        const templateId = knownVaults.get(path)
        if (templateId === undefined) {
          throw `路径不是有效的 Corpus Vault：${path}（无法读取 ${path}/.corpus/vault.json（浏览器模式：目录不在已知 Vault 集合内））`
        }
        const template = TEMPLATES.find((t) => t.id === templateId) ?? TEMPLATES[0]!
        upsertRecent(path, templateId)
        return { root: path, template_id: templateId, template_name: template.name }
      }
      case 'list_pages': {
        const { vaultPath } = args as { vaultPath: string }
        if (!knownVaults.has(vaultPath)) {
          throw `路径不是有效的 Corpus Vault：${vaultPath}（缺 wiki/ 目录）`
        }
        return fakePageTree()
      }
      case 'read_page': {
        const { vaultPath, relPath } = args as { vaultPath: string; relPath: string }
        if (!knownVaults.has(vaultPath)) {
          throw `路径不是有效的 Corpus Vault：${vaultPath}（缺 wiki/ 目录）`
        }
        if (relPath.includes('..') || !relPath.endsWith('.md')) {
          throw `非法页面路径：${relPath}（必须是 wiki/ 内的相对 .md 路径）`
        }
        const known = fakePageTree().groups.some((g) => g.pages.some((p) => p.path === relPath))
        if (!known) {
          throw `读取 ${vaultPath}/wiki/${relPath} 失败：文件不存在（浏览器模式假实现）`
        }
        return fakePageContent(relPath)
      }
      case 'ingest_file': {
        const { rawPath } = args as { rawPath: string }
        return fakeIngest(rawPath, DEFAULT_CONFIG.apply_mode)
      }
      // 对话框：目录选择返回演示 Vault；文件选择返回演示 raw 文件
      case 'plugin:dialog|open': {
        const { options } = args as { options?: { directory?: boolean } }
        return options?.directory ? '/tmp/demo-vault' : '/tmp/demo-vault/raw/paper.md'
      }
      default:
        throw `浏览器模式未实现该命令的 mock：${cmd}`
    }
  },
  { shouldMockEvents: true },
)
