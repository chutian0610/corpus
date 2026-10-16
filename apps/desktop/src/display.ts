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

/** provider id → 中文展示名。
 *
 * 这是引擎 config.rs 中 PROVIDER_SPECS 单一数据源的**展示层镜像**
 * （同 TEMPLATE_DISPLAY_NAMES 模式）：id 与顺序须与引擎侧保持一致；
 * 引擎新增 provider 时此处同步补展示名，展示逻辑不回灌引擎。 */
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

/** 路径最后一段（目录名 / 文件名），兼容 Windows 分隔符 */
export function pathBasename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean)
  return parts.length > 0 ? parts[parts.length - 1] : path
}

/** 页面路径（相对 wiki/）的展示名：末段文件名去 .md 后缀 */
export function pageDisplayName(path: string): string {
  return pathBasename(path).replace(/\.md$/, '')
}

/** 把 Tauri invoke 抛出的错误统一转为可展示文本。
 *
 * Tauri 命令错误多为字符串，但结构化错误会以对象抛出——统一收敛到
 * 这一出口，页面不再各自 `typeof err === 'string'` 判断。 */
export function errorMessage(err: unknown): string {
  return typeof err === 'string' ? err : JSON.stringify(err)
}
