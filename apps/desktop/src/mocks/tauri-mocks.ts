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
 * Ingest 流程修正：ingest_file 先假导入（任意位置 → raw/，内存账本
 * 去重 / 后缀递增），文件对话框返回 Vault 外路径（不再引导进 raw/）。
 * 进度透明度：分析/生成两步用假流式按 ~300ms 间隔吐递增 chars（镜像
 * 引擎流式上报的负载形状），浏览器模式能看到步骤卡上的字数跳动。
 *
 * 事件：mockIPC 开启 shouldMockEvents 后，前端 `emit` 会触达 `listen`
 * 注册的回调——ingest_file 的假进度借此走真实事件路径（不落盘）。
 */
import { mockIPC } from '@tauri-apps/api/mocks'
import type { InvokeArgs } from '@tauri-apps/api/core'
import type { PageEntry, PageFrontmatter, ReadPageResult } from '../api'

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

interface LlmCallStats {
  ts: string
  tier: 'fast' | 'strong'
  purpose: 'analyze' | 'compose'
  attempt: number
  system_chars: number
  user_chars: number
  output_chars: number
  duration_ms: number
  outcome: 'ok' | 'parse_failed' | 'oversized' | 'error'
  error?: string
}

interface IngestReport {
  skipped: boolean
  imported_raw_path: string
  newly_imported: boolean
  applied: boolean
  fingerprint: string
  source_title: string
  pages: IngestedPage[]
  commit: string | null
  summary_lines: string[]
  note: string | null
  llm_calls: LlmCallStats[]
}

type IngestProgress =
  | { stage: 'reading'; raw: string }
  | { stage: 'analyzing'; chars: number }
  | { stage: 'generating'; pages: number; chars: number }
  | { stage: 'applying'; pages: number }

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
  fast: { provider: 'openai', model: '', base_url: null, api_key: null },
  strong: { provider: 'openai', model: '', base_url: null, api_key: null },
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

/** 假页面树：基础三页 + 假摄取（auto 模式）成功后追加的 source 页。
 *  条目携带 frontmatter title / aliases（T4 wikilink 解析数据源）。 */
let extraSourcePages: PageEntry[] = []

function fakePageTree(): PageTree {
  const updated = '2026-10-16T10:30:00+08:00'
  return {
    groups: [
      {
        kind: 'source',
        pages: [
          {
            path: 'sources/attention-is-all-you-need.md',
            updated,
            title: 'Attention Is All You Need',
            aliases: ['Attention 论文'],
          },
          ...extraSourcePages,
        ],
      },
      {
        kind: 'entity',
        pages: [
          { path: 'entities/transformer.md', updated, title: 'Transformer', aliases: ['Transformer 架构'] },
        ],
      },
      {
        kind: 'concept',
        pages: [
          { path: 'concepts/自注意力.md', updated, title: '自注意力', aliases: ['Self-Attention'] },
        ],
      },
    ],
  }
}

/** 假页面（read_page_typed）：与假页面树一一对应的结构化 frontmatter +
 *  正文；index.md（引擎自维护目录，无 frontmatter）宽松降级。 */
function fakePageTyped(relPath: string): ReadPageResult {
  if (relPath === 'index.md') {
    return {
      frontmatter: null,
      body: [
        '# Wiki Index',
        '',
        '- [[sources/attention-is-all-you-need|Attention Is All You Need]]',
        '- [[entities/transformer|Transformer]]',
        '- [[concepts/自注意力]]',
        '- [[concepts/不存在的页面|死链示例]]',
        '',
      ].join('\n'),
    }
  }
  const titles: Record<string, string> = {
    'sources/attention-is-all-you-need.md': 'Attention Is All You Need',
    'entities/transformer.md': 'Transformer',
    'concepts/自注意力.md': '自注意力',
  }
  const title = titles[relPath] ?? relPath
  const kind = relPath.startsWith('sources/')
    ? 'source'
    : relPath.startsWith('entities/')
      ? 'entity'
      : 'concept'
  const frontmatter: PageFrontmatter = {
    type: kind,
    title,
    aliases: [title],
    tags: kind === 'entity' ? ['deep-learning'] : [],
    related: kind === 'entity' ? ['[[concepts/自注意力]]'] : [],
    sources: kind === 'concept' ? [] : ['[[sources/attention-is-all-you-need]]'],
    created: '2026-10-16T10:30:00+08:00',
    updated: '2026-10-16T10:30:00+08:00',
    raw_path: kind === 'source' ? 'raw/paper.md' : null,
    fingerprint: kind === 'source' ? 'browser-mock-fingerprint' : null,
  }
  return {
    frontmatter,
    body: [
      `# ${title}`,
      '',
      '（浏览器模式假页面内容。）',
      '相关页面：[[entities/transformer|Transformer]]、[[concepts/自注意力]]。',
      `这是一段死链：[[concepts/幽灵页面|不存在的页面]]。`,
      '',
      '```rust',
      'fn main() { println!("代码块里的 [[entities/transformer]] 不是链接"); }',
      '```',
      '',
    ].join('\n'),
  }
}

/** read_page 的路径校验（引擎 resolve_page_path 语义的 JS 简化版）：
 *  拒绝绝对路径（前导 `/`、`\` 或 Windows 盘符 `C:`）、段级 `..` 与
 *  非 `.md` 后缀。段级判断只认整段等于 `..`，不误伤 `a..b.md` 这类
 *  文件名（引擎侧由逐组件校验等价保证；symlink 逃逸在浏览器 mock 无
 *  文件系统，无从模拟）。 */
function isValidPagePath(relPath: string): boolean {
  const isAbsolute = /^(\/|\\|[A-Za-z]:)/.test(relPath)
  const hasParentSegment = relPath
    .split(/[\\/]/)
    .some((segment) => segment === '..')
  return !isAbsolute && !hasParentSegment && relPath.endsWith('.md')
}

/** 「导入账本」：源路径 → 已占用的 raw 文件名（浏览器模式的内存态）。 */
const importedRawBySrc = new Map<string, string>()

/**
 * 假导入（引擎 import_raw 语义的浏览器简化版）：任意位置的源文件
 * 「复制」进 raw/（内存记账，不写文件系统）——文件名保留；同一源
 * 再次导入去重（newly_imported=false）；不同源同名（视为不同内容）
 * 则后缀递增（foo.md → foo-2.md）。无文件系统，无从比对内容哈希，
 * 以「同一源路径」近似「同内容」。
 */
function fakeImport(srcPath: string): { imported_raw_path: string; newly_imported: boolean } {
  const known = importedRawBySrc.get(srcPath)
  if (known !== undefined) {
    return { imported_raw_path: known, newly_imported: false }
  }
  const name = srcPath.split('/').pop() ?? 'untitled.md'
  const stem = name.replace(/(\.[^.]+)$/, '')
  const ext = name.match(/(\.[^.]+)$/)?.[1] ?? ''
  let candidate = name
  let counter = 1
  while ([...importedRawBySrc.values()].includes(candidate)) {
    counter += 1
    candidate = `${stem}-${counter}${ext}`
  }
  importedRawBySrc.set(srcPath, `raw/${candidate}`)
  return { imported_raw_path: `raw/${candidate}`, newly_imported: true }
}

/**
 * 假流式（镜像引擎流式 chars 上报的形状）：按 ~12 段把 total 字吐完，
 * 每段间隔 stepMs——浏览器模式能看到步骤卡上的字数跳动（引擎侧真实
 * 节流是 200ms / 50 字，这里取整为等距分段便于演示）。
 */
async function emitStreamedChars(
  stage: 'analyzing' | 'generating',
  total: number,
  stepMs: number,
  pages?: number,
): Promise<void> {
  const { emit } = await import('@tauri-apps/api/event')
  const step = Math.max(1, Math.ceil(total / 12))
  for (let chars = step; ; chars = Math.min(total, chars + step)) {
    await new Promise((resolve) => setTimeout(resolve, stepMs))
    const payload: IngestProgress =
      stage === 'analyzing' ? { stage, chars } : { stage, pages: pages ?? 0, chars }
    await emit('ingest-progress', payload)
    if (chars >= total) break
  }
}

/**
 * 假摄取（模拟默认配置 apply_mode=manual）：先假导入（任意位置 →
 * raw/），再推进进度事件（走 mock 事件通道；分析/生成两步用假流式
 * 吐递增 chars，人审模式无落盘阶段），最后 resolve 未落盘的 Draft
 * 检视清单。不写任何文件；apply_mode=auto 时模拟落盘成功并让页面树
 * 长出一页 source。
 */
async function fakeIngest(srcPath: string, applyMode: string): Promise<IngestReport> {
  const { imported_raw_path, newly_imported } = fakeImport(srcPath)
  const { emit } = await import('@tauri-apps/api/event')
  await new Promise((resolve) => setTimeout(resolve, 400))
  await emit('ingest-progress', { stage: 'reading', raw: imported_raw_path })
  // 第一步（分析）：假流式增量
  await new Promise((resolve) => setTimeout(resolve, 300))
  await emit('ingest-progress', { stage: 'analyzing', chars: 0 })
  await emitStreamedChars('analyzing', 512, 300)
  // 第二步（生成）：同上，携带计划页面数
  await new Promise((resolve) => setTimeout(resolve, 300))
  await emit('ingest-progress', { stage: 'generating', pages: FAKE_INGEST_PAGES.length, chars: 0 })
  await emitStreamedChars('generating', 1836, 300, FAKE_INGEST_PAGES.length)
  const now = new Date().toISOString()
  const llm_calls: LlmCallStats[] = [
    { ts: now, tier: 'fast', purpose: 'analyze', attempt: 1, system_chars: 420, user_chars: 8_512, output_chars: 1_864, duration_ms: 2_310, outcome: 'ok' },
    { ts: now, tier: 'strong', purpose: 'compose', attempt: 1, system_chars: 380, user_chars: 9_120, output_chars: 4_437, duration_ms: 6_780, outcome: 'ok' },
  ]
  if (applyMode === 'auto') {
    await new Promise((resolve) => setTimeout(resolve, 400))
    await emit('ingest-progress', { stage: 'applying', pages: FAKE_INGEST_PAGES.length })
    // 模拟落盘：页面树长出一页 source（真实引擎由文件系统驱动）
    const stamp = new Date().toISOString()
    const rawName = imported_raw_path.split('/').pop() ?? 'browser-mock.md'
    extraSourcePages = [
      ...extraSourcePages,
      {
        path: `sources/${rawName.replace(/\.(md|markdown|txt)$/, '')}.md`,
        updated: stamp,
        title: rawName.replace(/\.(md|markdown|txt)$/, ''),
        aliases: [],
      },
    ]
    return {
      skipped: false,
      imported_raw_path,
      newly_imported,
      applied: true,
      fingerprint: 'browser-mock-fingerprint',
      source_title: 'Attention Is All You Need',
      pages: FAKE_INGEST_PAGES,
      commit: 'abc1234',
      summary_lines: [],
      note: null,
      llm_calls,
    }
  }
  return {
    skipped: false,
    imported_raw_path,
    newly_imported,
    applied: false,
    fingerprint: 'browser-mock-fingerprint',
    source_title: 'Attention Is All You Need',
    pages: FAKE_INGEST_PAGES,
    commit: null,
    summary_lines: [
      `source: Attention Is All You Need (raw: ${imported_raw_path}, sha256: browser-mock-fingerprint)`,
      'Create sources/attention-is-all-you-need.md: Attention Is All You Need',
      'Create entities/transformer.md: Transformer',
      'Create concepts/自注意力.md: 自注意力',
    ],
    note: '人审模式：Draft 未落盘，完整人审流程属 T5',
    llm_calls,
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
      case 'read_page_typed': {
        const { vaultPath, relPath } = args as { vaultPath: string; relPath: string }
        if (!knownVaults.has(vaultPath)) {
          throw `路径不是有效的 Corpus Vault：${vaultPath}（缺 wiki/ 目录）`
        }
        if (!isValidPagePath(relPath)) {
          throw `非法页面路径：${relPath}（必须是 wiki/ 内的相对 .md 路径）`
        }
        const known =
          relPath === 'index.md' ||
          fakePageTree().groups.some((g) => g.pages.some((p) => p.path === relPath))
        if (!known) {
          throw `读取 ${vaultPath}/wiki/${relPath} 失败：文件不存在（浏览器模式假实现）`
        }
        return fakePageTyped(relPath)
      }
      case 'ingest_file': {
        const { rawPath } = args as { rawPath: string }
        return fakeIngest(rawPath, DEFAULT_CONFIG.apply_mode)
      }
      // 对话框：目录选择返回演示 Vault；文件选择返回 Vault 外的演示
      // 文档（Ingest 流程「任意位置选文件 → 自动导入 raw/」，对话框
      // 不再引导进 raw/）
      case 'plugin:dialog|open': {
        const { options } = args as { options?: { directory?: boolean } }
        return options?.directory ? '/tmp/demo-vault' : '/Users/demo/Downloads/paper.md'
      }
      default:
        throw `浏览器模式未实现该命令的 mock：${cmd}`
    }
  },
  { shouldMockEvents: true },
)
