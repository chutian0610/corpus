/**
 * 前端访问后端的唯一入口。
 *
 * 组件不直接 invoke——所有命令调用与响应类型收敛在此。
 * 当前 transport 是 Tauri IPC；计划中的本地 web 形态（见 GitHub 票据）
 * 只需替换本模块的实现为 HTTP fetch，组件层零改动。
 *
 * 响应字段均为引擎 serde 的 snake_case。
 */
import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

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
  return invoke<TemplateInfo[]>('list_templates')
}

export function createVault(path: string, templateId: string): Promise<VaultInfo> {
  return invoke<VaultInfo>('create_vault', { path, templateId })
}

export function getConfig(): Promise<AppConfig> {
  return invoke<AppConfig>('get_config')
}

export function setConfig(config: AppConfig): Promise<AppConfig> {
  return invoke<AppConfig>('set_config', { config })
}

export function testConnection(tier: ModelTier): Promise<ConnectionReport> {
  return invoke<ConnectionReport>('test_connection', { tier })
}

export function listRecentVaults(): Promise<RecentVault[]> {
  return invoke<RecentVault[]>('list_recent_vaults')
}

export function removeRecentVault(path: string): Promise<void> {
  return invoke('remove_recent_vault', { path })
}

/** 打开一个已有 Vault：引擎校验 + 壳层登记最近列表，非 Vault 明确报错。 */
export function openVault(path: string): Promise<OpenedVault> {
  return invoke<OpenedVault>('open_vault', { path })
}

/** 当前 Vault 的 wiki/ 页面树（文件面板数据源）。 */
export function listPages(vaultPath: string): Promise<PageTree> {
  return invoke<PageTree>('list_pages', { vaultPath })
}

/** 只读读取 wiki/ 下某页面的原始文本（预览用；正式渲染属 T4）。 */
export function readPage(vaultPath: string, relPath: string): Promise<string> {
  return invoke<string>('read_page', { vaultPath, relPath })
}

/** 摄取一个文档（任意位置）：命令内先 import_raw 导入 raw/，再走 Ingest 管道。 */
export function ingestFile(vaultPath: string, rawPath: string): Promise<IngestReport> {
  return invoke<IngestReport>('ingest_file', { vaultPath, rawPath })
}

/**
 * 订阅 Ingest 进度事件；返回的 unlisten 必须在组件卸载时调用
 * （SPA 路由切换不会自动注销，见 T3 调研笔记第 4 节）。
 */
export function onIngestProgress(handler: (payload: IngestProgress) => void): Promise<UnlistenFn> {
  return listen<IngestProgress>('ingest-progress', (e) => handler(e.payload))
}
