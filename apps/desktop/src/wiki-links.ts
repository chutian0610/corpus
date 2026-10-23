/**
 * Wiki 页面渲染（票据 #5 T4）：wikilink 目标解析与 Markdown → HTML。
 *
 * 安全模型（严禁无转义直插 v-html）：
 * - wikilink 用 markdown-it inline rule 在 **tokenize 阶段**生成受控
 *   `<a>` token——data-target 与链接文本都经 markdown-it 渲染器的属性 /
 *   文本转义输出，页面里的任何字符都不可能逃出标签或注入新标签；
 * - markdown-it 实例保持 html:false（默认）——源文本中的手写 HTML 一律
 *   转义为字面文本展示（防 XSS；引擎生成的页面不含 HTML，展示成字面
 *   文本无妨）；token 层生成的 `<a>` 不受 html:false 影响；
 * - inline rule 天然尊重代码块：反引号内的 `[[...]]` 由 backticks 规则
 *   先行消费，不会被链接化；
 * - resolver 只做映射查找，结果仅用于决定活/死链样式与跳转，不进 HTML。
 */
import MarkdownItCallable from 'markdown-it'
import type { StateInline } from 'markdown-it'

/** wikilink 目标解析所需的最小页面信息（api.ts PageEntry 的子集）。 */
export interface PageEntryLike {
  path: string
  title: string | null
  aliases: string[]
}

/** wikilink target → 页面路径（相对 wiki/、含 .md）；解析不到返回 null。 */
export type WikiLinkResolver = (target: string) => string | null

/** 名称归一化：去首尾空白 + 小写（与引擎 ingest::normalize_name 同口径，
 *  精确名匹配不区分大小写；中文不受影响）。 */
function normalizeName(name: string): string {
  return name.trim().toLowerCase()
}

/** 页面路径 → wikilink link 形式（去 .md 后缀，与引擎 PageDraft::link 一致）。 */
function linkForm(path: string): string {
  return path.replace(/\.md$/, '')
}

/**
 * 由页面树条目构建 wikilink 解析器：路径精确 → title 精确 → alias，
 * 三级映射各按归一化 key 查找（首见优先，避免歧义名来回覆盖）。
 * 与引擎 resolve 口径一致的归一化保证 LLM 生成的 `[[title]]` /
 * `[[path]]` / `[[alias]]` 三种形态都能命中。
 */
export function buildResolver(entries: readonly PageEntryLike[]): WikiLinkResolver {
  const byPath = new Map<string, string>()
  const byTitle = new Map<string, string>()
  const byAlias = new Map<string, string>()
  for (const entry of entries) {
    byPath.set(normalizeName(linkForm(entry.path)), entry.path)
    byPath.set(normalizeName(entry.path), entry.path)
    if (entry.title !== null && !byTitle.has(normalizeName(entry.title))) {
      byTitle.set(normalizeName(entry.title), entry.path)
    }
    for (const alias of entry.aliases) {
      const key = normalizeName(alias)
      if (!byAlias.has(key)) byAlias.set(key, entry.path)
    }
  }
  return (target) => {
    const key = normalizeName(target)
    return byPath.get(key) ?? byTitle.get(key) ?? byAlias.get(key) ?? null
  }
}

/** markdown-it 实例（模块级单例；html:false 是安全边界的一部分）。 */
const md = new MarkdownItCallable({ html: false, linkify: false, breaks: false })

/**
 * wikilink inline rule：`[[target]]` / `[[target|alias]]` → 受控 `<a>`。
 * 注册进 inline.ruler 末尾（backticks 之后），代码块内不生效。
 */
function wikilinkRule(state: StateInline, silent: boolean): boolean {
  const src = state.src
  const pos = state.pos
  if (src.charCodeAt(pos) !== 0x5b || src.charCodeAt(pos + 1) !== 0x5b) return false // `[[`
  const end = src.indexOf(']]', pos + 2)
  if (end === -1) return false
  const inner = src.slice(pos + 2, end)
  if (inner.includes('[')) return false // 嵌套方括号让位给普通 Markdown 语法
  const sep = inner.indexOf('|')
  const target = (sep === -1 ? inner : inner.slice(0, sep)).trim()
  if (target === '') return false
  const label = sep === -1 ? inner : inner.slice(sep + 1)
  const labelTrimmed = label.trim() === '' ? target : label.trim()

  if (!silent) {
    // resolve 由 renderWikiMarkdown 经 env 必传入（唯一渲染入口），不可能是
    // undefined，无需兜底分支。
    const resolve = state.env.resolve as WikiLinkResolver
    const resolved = resolve(target)
    const open = state.push('wikilink_open', 'a', 1)
    open.attrs = [
      ['class', resolved === null ? 'wikilink dead' : 'wikilink'],
      ['data-target', target],
      ['title', resolved === null ? `页面不存在：${target}` : target],
    ]
    const text = state.push('text', '', 0)
    text.content = labelTrimmed
    state.push('wikilink_close', 'a', -1)
  }
  state.pos = end + 2
  return true
}
md.inline.ruler.push('wikilink', wikilinkRule)

/**
 * Markdown 正文 → HTML（wikilink 已解析为 a.wikilink；死链带 dead 类）。
 *
 * `resolve` 经 render 的 env 传入 inline rule（必传参，inline rule 直接
 * 调用，无 undefined 兜底分支）。
 */
export function renderWikiMarkdown(src: string, resolve: WikiLinkResolver): string {
  return md.render(src, { resolve })
}

/** frontmatter related/sources 条目（wikilink 形式）的可展示形态。 */
export interface MetaLink {
  /** wikilink 目标（去 [[]] 与别名部分）。 */
  target: string
  /** 展示文本（别名优先，无别名用目标）。 */
  label: string
}

/** 解析 frontmatter 数组字段的单个条目：`[[target|label]]` / `[[target]]` /
 *  裸字符串（LLM 偶发不合规形态，宽容展示为裸文本）。 */
export function parseMetaLink(item: string): MetaLink {
  const m = item.match(/^\[\[([^[\]|]+)(?:\|([^[\]]*))?\]\]$/)
  if (m === null) return { target: item.trim(), label: item.trim() }
  const target = (m[1] ?? '').trim()
  const label = (m[2] ?? '').trim() || target
  return { target, label }
}
