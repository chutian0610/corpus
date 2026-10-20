/**
 * 前端访问后端的唯一入口。
 *
 * 组件不直接 invoke——所有命令调用与响应类型收敛在此。
 * 双 transport（票据 #17 v1 本地 web 形态）按运行环境自动选择：
 * - Tauri 壳内（window.__TAURI_INTERNALS__ 存在）：走 invoke（IPC）；
 * - 浏览器（corpus-server 静态服务或 vite dev 代理）：走同源
 *   fetch('/api/...') + X-Corpus-Token 头（token 在启动时经 ?token=
 *   引导进 sessionStorage，见 main.ts）。
 * 组件层零改动：导出的函数签名与响应字段形状完全不变。
 *
 * 响应字段均为引擎 serde 的 snake_case。
 */
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

/** 是否运行在 Tauri 壳内（壳内走 IPC，浏览器走 HTTP）。 */
export const isTauri = typeof (window as any).__TAURI_INTERNALS__ !== 'undefined'

/** SSE 进度事件名（与 corpus-server 的 INGEST_PROGRESS_SSE_EVENT 对应）。 */
const INGEST_PROGRESS_SSE_EVENT = 'ingest-progress'

/** token 在 sessionStorage 的键（main.ts 启动时从 ?token= 引导进来）。 */
export const CORPUS_TOKEN_KEY = 'corpus-token'

export function readToken(): string {
  return sessionStorage.getItem(CORPUS_TOKEN_KEY) ?? ''
}

export function storeToken(token: string): void {
  sessionStorage.setItem(CORPUS_TOKEN_KEY, token)
}

// ---------------------------------------------------------------------------
// HTTP transport（浏览器壳）：同源 fetch + token 头
// ---------------------------------------------------------------------------

function tokenHeaders(): Record<string, string> {
  return isTauri ? {} : { 'X-Corpus-Token': readToken() }
}

/**
 * 统一 HTTP 调用：401 时 console 明确指路（从 corpus-server 启动输出
 * 复制带 token 的 URL 重开）；非 2xx 把服务端 {message} 中文错误体转
 * 为 reject。**reject 的是字符串**——与 Tauri invoke 的错误形态一致，
 * display.ts 的 errorMessage() 双壳同一口径。
 */
async function httpJson<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: { ...tokenHeaders(), ...(init?.headers ?? {}) },
  })
  if (res.status === 401) {
    console.error(
      'Corpus：请求被拒绝（401，令牌缺失或无效）。请从 corpus-server 启动输出复制完整 URL（含 ?token=）重新打开本页。',
    )
    throw '需要访问令牌：请从 corpus-server 启动输出的 URL 打开本页'
  }
  if (!res.ok) {
    let message = `HTTP ${res.status}`
    try {
      const body = await res.json()
      if (body?.message) message = body.message
    } catch {
      // 非 JSON 错误体：保持状态码文案
    }
    throw message
  }
  return (await res.json()) as T
}

function httpPost<T>(path: string, body: unknown): Promise<T> {
  return httpJson<T>(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  })
}

export interface TemplateInfo {
  id: string
  name: string
  description: string
}

export interface VaultInfo {
  root: string
  template_id: string
  template_name: string
  created_paths: string[]
}

/** open_vault 的结果：路径与绑定模板元数据（引擎 OpenedVault）。 */
export interface OpenedVault {
  root: string
  template_id: string
  template_name: string
}

export interface ModelTier {
  provider: string
  model: string
  base_url: string | null
  api_key_env: string | null
}

export interface AppConfig {
  fast: ModelTier
  strong: ModelTier
  apply_mode: 'auto' | 'manual'
  query_pages: boolean
}

export interface ConnectionReport {
  latency_ms: number
  reply: string
}

export interface RecentVault {
  path: string
  template_id: string
  opened_at: string
}

/** 页面类型（引擎 PageKind serde 表示）。 */
export type PageKind = 'source' | 'entity' | 'concept'

/** 单个页面条目（引擎 PageEntry）：路径 + 最后更新时间。 */
export interface PageEntry {
  /** 相对 wiki/ 的 .md 路径。 */
  path: string
  /** frontmatter 的 updated 时间（RFC 3339）；解析失败为 null（界面不展示）。 */
  updated: string | null
}

/** 按类型分组的页面清单（引擎 PageGroup）。 */
export interface PageGroup {
  kind: PageKind
  pages: PageEntry[]
}

/** wiki/ 页面树（引擎 PageTree）：非空分组，按固定类型顺序。 */
export interface PageTree {
  groups: PageGroup[]
}

/** 页面动作：新建 / 精确名命中更新。 */
export type PageAction = 'create' | 'update'

/** Ingest 产出 / 更新的单个页面。 */
export interface IngestedPage {
  path: string
  title: string
  page_type: PageKind
  action: PageAction
}

/** LLM 档位（fast 分析 / strong 成文）。 */
export type LlmTier = 'fast' | 'strong'

/** LLM 调用用途。 */
export type LlmCallPurpose = 'analyze' | 'compose'

/** 单次 LLM 调用的结果分类。 */
export type LlmCallOutcome = 'ok' | 'parse_failed' | 'oversized' | 'error'

/** 单次 LLM 调用统计（引擎 LlmCallStats；无 prompt/输出原文，只有长度与耗时）。 */
export interface LlmCallStats {
  /** 调用完成时刻（RFC 3339）。 */
  ts: string
  tier: LlmTier
  purpose: LlmCallPurpose
  /** 第几次尝试（1 起；解析失败 / 超限会重试一次）。 */
  attempt: number
  system_chars: number
  user_chars: number
  output_chars: number
  duration_ms: number
  outcome: LlmCallOutcome
  /** 失败原因摘要（成功时无此字段）。 */
  error?: string
}

/** ingest_file 的结果：导入信息 + 跳过路径（指纹命中）、已落盘或人审未落盘的快照。 */
export interface IngestReport {
  skipped: boolean
  /** 导入后的 raw 文件相对 Vault 路径（如 raw/foo.md；引擎 import_raw 结果，管道摄取的就是它）。 */
  imported_raw_path: string
  /** 本次是否新导入进 raw/（false = 已在库或同内容去重命中既有文件）。 */
  newly_imported: boolean
  /** Draft 是否已落盘（指纹跳过与人审模式为 false）。 */
  applied: boolean
  fingerprint: string
  source_title: string
  pages: IngestedPage[]
  commit: string | null
  /** 人审模式（apply_mode=manual）：Draft 人读检视清单。 */
  summary_lines: string[]
  /** 人审模式注明（Draft 未落盘，完整人审流程属 T5）。 */
  note: string | null
  /** 本次摄取的全部 LLM 调用统计（分析 + 成文，含重试；跳过路径为空）。 */
  llm_calls: LlmCallStats[]
}

/** Ingest 进度事件 payload（引擎 IngestStage，tag=stage）。
 *
 *  分析/生成两步是 LLM 流式：同阶段重复事件携带递增 chars（模型已输出
 *  字符数，引擎侧已节流合并），chars=0 表示阶段开始。 */
export type IngestProgress =
  | { stage: 'reading'; raw: string }
  | { stage: 'analyzing'; chars: number }
  | { stage: 'generating'; pages: number; chars: number }
  | { stage: 'applying'; pages: number }

export function listTemplates(): Promise<TemplateInfo[]> {
  if (isTauri) return invoke<TemplateInfo[]>('list_templates')
  return httpJson<TemplateInfo[]>('/api/templates')
}

export function createVault(path: string, templateId: string): Promise<VaultInfo> {
  if (isTauri) return invoke<VaultInfo>('create_vault', { path, templateId })
  return httpPost<VaultInfo>('/api/vaults', { path, template_id: templateId })
}

export function getConfig(): Promise<AppConfig> {
  if (isTauri) return invoke<AppConfig>('get_config')
  return httpJson<AppConfig>('/api/config')
}

export function setConfig(config: AppConfig): Promise<AppConfig> {
  if (isTauri) return invoke<AppConfig>('set_config', { config })
  return httpJson<AppConfig>('/api/config', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(config),
  })
}

export function testConnection(tier: ModelTier): Promise<ConnectionReport> {
  if (isTauri) return invoke<ConnectionReport>('test_connection', { tier })
  return httpPost<ConnectionReport>('/api/config/test-connection', tier)
}

export function listRecentVaults(): Promise<RecentVault[]> {
  if (isTauri) return invoke<RecentVault[]>('list_recent_vaults')
  return httpJson<RecentVault[]>('/api/vaults/recent')
}

export function removeRecentVault(path: string): Promise<void> {
  if (isTauri) return invoke('remove_recent_vault', { path })
  // path 经 query 传（server 端取舍：DELETE body 的客户端/代理兼容性差）
  return httpJson<void>(`/api/vaults/recent?path=${encodeURIComponent(path)}`, {
    method: 'DELETE',
  })
}

/** 打开一个已有 Vault：引擎校验 + 壳层登记最近列表，非 Vault 明确报错。 */
export function openVault(path: string): Promise<OpenedVault> {
  if (isTauri) return invoke<OpenedVault>('open_vault', { path })
  return httpPost<OpenedVault>('/api/vaults/open', { path })
}

/** 当前 Vault 的 wiki/ 页面树（文件面板数据源）。 */
export function listPages(vaultPath: string): Promise<PageTree> {
  if (isTauri) return invoke<PageTree>('list_pages', { vaultPath })
  return httpJson<PageTree>(`/api/vaults/pages?vault=${encodeURIComponent(vaultPath)}`)
}

/** 只读读取 wiki/ 下某页面的原始文本（预览用；正式渲染属 T4）。 */
export function readPage(vaultPath: string, relPath: string): Promise<string> {
  if (isTauri) return invoke<string>('read_page', { vaultPath, relPath })
  return httpJson<string>(
    `/api/vaults/page?vault=${encodeURIComponent(vaultPath)}&rel=${encodeURIComponent(relPath)}`,
  )
}

/** 摄取一个文档（任意位置）：先 import_raw 导入 raw/，再走 Ingest 管道。
 *
 * 浏览器 transport：POST /api/ingest 阻塞到摄取完成，响应即完整报告
 * （进度不经本响应，走 onIngestProgress 的 SSE）。 */
export function ingestFile(vaultPath: string, rawPath: string): Promise<IngestReport> {
  if (isTauri) return invoke<IngestReport>('ingest_file', { vaultPath, rawPath })
  return httpPost<IngestReport>('/api/ingest', { vault_path: vaultPath, src_path: rawPath })
}

/**
 * 订阅 Ingest 进度事件；返回的 unlisten 必须在组件卸载时调用
 * （SPA 路由切换不会自动注销，见 T3 调研笔记第 4 节）。
 *
 * 浏览器 transport：EventSource（SSE）。EventSource 无法自定义请求头，
 * token 只能走 ?token= 查询参数（server 端仅对 /api/events 放行该
 * 方式，取舍见 server token_auth 注释）；unlisten = 关闭连接。
 */
export function onIngestProgress(handler: (payload: IngestProgress) => void): Promise<UnlistenFn> {
  if (isTauri) {
    return listen<IngestProgress>('ingest-progress', (e) => handler(e.payload))
  }
  const source = new EventSource(`/api/events?token=${encodeURIComponent(readToken())}`)
  source.addEventListener(INGEST_PROGRESS_SSE_EVENT, (ev) => {
    try {
      handler(JSON.parse((ev as MessageEvent).data as string) as IngestProgress)
    } catch (err) {
      console.error('Corpus：进度事件解析失败', err)
    }
  })
  source.onerror = () => {
    // EventSource 失败后会自动重连；连接被拒（401 / 服务未起）时给
    // 出与 401 相同的指路提示，不打断 UI。
    console.error(
      'Corpus：进度事件流连接失败。请确认 corpus-server 在运行、令牌有效（从启动输出复制 URL 重开）。',
    )
  }
  const unlisten: UnlistenFn = () => source.close()
  // Tauri 的 listen 返回的 Promise 在监听注册后 resolve；SSE 没有等价
  // 的「注册完成」信号，立即 resolve unlisten 即可（连接在后台建立，
  // 早到的进度事件有组件级 loading 态兜底）。
  return Promise.resolve(unlisten)
}
