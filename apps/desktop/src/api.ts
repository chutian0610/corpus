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

export function addRecentVault(path: string, templateId: string): Promise<void> {
  return invoke('add_recent_vault', { path, templateId })
}

export function removeRecentVault(path: string): Promise<void> {
  return invoke('remove_recent_vault', { path })
}
