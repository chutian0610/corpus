/** 展示层翻译：模板 id → 中文名。id 集合与清单以引擎下发的 TemplateInfo 为准 */

export const TEMPLATE_DISPLAY_NAMES: Record<string, string> = {
  generic: '通用知识库',
  research: '研究',
  reading: '阅读',
}

/** 展示名优先取中文映射，无映射时回退引擎下发的 name */
export function templateDisplayName(id: string, name?: string): string {
  return TEMPLATE_DISPLAY_NAMES[id] ?? name ?? id
}

/** provider id → 中文展示名（ProviderKind serde 值，见引擎 config.rs） */
export const PROVIDER_OPTIONS: ReadonlyArray<{ id: string; label: string }> = [
  { id: 'openai', label: 'OpenAI' },
  { id: 'anthropic', label: 'Anthropic' },
  { id: 'deepseek', label: 'DeepSeek' },
  { id: 'openai_compatible', label: 'OpenAI 兼容端点' },
]

/** RFC3339 时间戳 → 本地化展示 */
export function formatDateTime(iso: string): string {
  const date = new Date(iso)
  return Number.isNaN(date.getTime()) ? iso : date.toLocaleString()
}
