<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import {
  ingestFile,
  isTauri,
  onIngestProgress,
  type IngestProgress,
  type IngestReport,
  type PageKind,
} from '../api'
import { errorMessage, PAGE_TYPE_LABELS } from '../display'
import PathPicker from './PathPicker.vue'
import { bumpPages, currentVault } from '../vault-state'

/** 四阶段步骤条（引擎 IngestStage 的 UI 映射）。 */
const STEPS: ReadonlyArray<{ key: IngestProgress['stage']; label: string }> = [
  { key: 'reading', label: '读取' },
  { key: 'analyzing', label: '分析' },
  { key: 'generating', label: '生成' },
  { key: 'applying', label: '落盘' },
]

type Phase = 'idle' | IngestProgress['stage'] | 'done' | 'error'
type StageKey = IngestProgress['stage']

/** 一个阶段的时间标记：开始时刻 + 结束时刻（进行中为 null）。 */
interface StageMark {
  key: StageKey
  start: number
  end: number | null
}

/** LLM 阶段无新增量的提示阈值（毫秒）：超过即显示「仍在等待模型响应」。 */
const STALL_HINT_THRESHOLD_MS = 15_000
/** 计时器跳动间隔（毫秒）：驱动「分析中 · 23s」与无进展提示的刷新。 */
const TICK_MS = 250

const srcPath = ref<string | null>(null)
const ingesting = ref(false)
const phase = ref<Phase>('idle')
const report = ref<IngestReport | null>(null)
const ingestError = ref<string | null>(null)
/** web 模式的服务器侧文件选择弹窗（Tauri 下弹系统对话框，不开本弹窗）。 */
const pickerOpen = ref(false)

/** 阶段时间线（按到达顺序；同阶段的 chars 更新不产生新标记）。 */
const stageMarks = ref<StageMark[]>([])
/** 跳动的当前时刻（setInterval 驱动，让耗时计算响应式刷新）。 */
const now = ref(Date.now())
/** 当前 LLM 阶段已接收字符数（引擎流式 chars，已节流）。 */
const llmChars = ref(0)
/** 最近一次 chars 增量到达时刻（无进展提示的计时起点）。 */
const lastDeltaAt = ref<number | null>(null)

let unlisten: (() => void) | null = null
let ticker: number | null = null

/** 阶段切换：闭合上一标记、开启新标记；LLM 阶段重置字数与等待计时。 */
function enterStage(stage: StageKey): void {
  const marks = stageMarks.value
  const last = marks[marks.length - 1]
  if (last !== undefined) {
    if (last.key === stage) return // 同阶段的 chars 更新，不产生新标记
    last.end = Date.now()
  }
  marks.push({ key: stage, start: Date.now(), end: null })
  llmChars.value = 0
  lastDeltaAt.value = Date.now()
}

/** 结束时间线（完成或失败）：定格最后阶段的耗时。 */
function closeStages(): void {
  const last = stageMarks.value[stageMarks.value.length - 1]
  if (last !== undefined && last.end === null) last.end = Date.now()
}

onMounted(async () => {
  ticker = window.setInterval(() => {
    now.value = Date.now()
  }, TICK_MS)
  // 进度事件：命令执行中由后端 emit；卸载必须 unlisten（调研笔记第 4 节）
  unlisten = await onIngestProgress((progress) => {
    if (!ingesting.value) return
    if (phase.value !== progress.stage) {
      phase.value = progress.stage
      enterStage(progress.stage)
    }
    if (progress.stage === 'analyzing' || progress.stage === 'generating') {
      if (progress.chars > llmChars.value) {
        llmChars.value = progress.chars
        lastDeltaAt.value = Date.now()
      }
    }
  })
})

onUnmounted(() => {
  unlisten?.()
  unlisten = null
  if (ticker !== null) window.clearInterval(ticker)
  ticker = null
})

/** 摄取只作用于当前打开的 Vault（工作区面板；无 Vault 时不会挂载）。 */
const vault = computed(() => currentVault.value)

const canIngest = computed(
  () => vault.value !== null && srcPath.value !== null && !ingesting.value,
)

/** 人审模式结果：Draft 未落盘（apply_mode=manual，完整人审流程属 T5）。 */
const manualPending = computed(
  () => report.value !== null && !report.value.skipped && !report.value.applied,
)

/** 当前步骤索引（done 时全部完成，idle 时 -1）。 */
const currentStep = computed(() => {
  if (phase.value === 'done') {
    // 人审模式未落盘：只有前三步（读取/分析/生成）真实完成
    if (manualPending.value) return 3
    return STEPS.length
  }
  const idx = STEPS.findIndex((s) => s.key === phase.value)
  return idx
})

/** 某步骤的时间标记（阶段单调推进，按 key 找首个即可）。 */
function markOf(idx: number): StageMark | null {
  const key = STEPS[idx]!.key
  return stageMarks.value.find((m) => m.key === key) ?? null
}

/** 标记的已耗时（进行中用跳动的 now；夹到 0——now 刻度可能略滞后于
 *  阶段开始的 Date.now，避免刚切换时显示 -1s）。 */
function elapsedOf(mark: StageMark): number {
  return Math.max(0, (mark.end ?? now.value) - mark.start)
}

/** 毫秒 → 人读时长（"23s" / "1m05s"）。 */
function formatDuration(ms: number): string {
  const totalSeconds = Math.floor(ms / 1000)
  if (totalSeconds < 60) return `${totalSeconds}s`
  const minutes = Math.floor(totalSeconds / 60)
  const seconds = totalSeconds % 60
  return `${minutes}m${String(seconds).padStart(2, '0')}s`
}

function isActiveStep(idx: number): boolean {
  return ingesting.value && idx === currentStep.value
}

/** 步骤主行：进行中显示跳动耗时（"分析中 · 23s"），完成后回显步骤名。 */
function stepTitle(idx: number): string {
  const label = STEPS[idx]!.label
  if (!isActiveStep(idx)) return label
  const mark = markOf(idx)
  return mark === null ? `${label}中` : `${label}中 · ${formatDuration(elapsedOf(mark))}`
}

/** 步骤副行：完成后定格该阶段总耗时；LLM 进行中只报字数不展示内容
 *  （分析输出是 JSON、生成输出会剧透 Wiki 正文，展示原文无意义）。 */
function stepSub(idx: number): string | null {
  if (isActiveStep(idx)) {
    if (phase.value === 'analyzing') return `已接收 ${llmChars.value} 字（模型输出中）`
    if (phase.value === 'generating') return `已生成 ${llmChars.value} 字（模型输出中）`
    return null
  }
  if (idx < currentStep.value) {
    const mark = markOf(idx)
    if (mark === null) return null
    return `耗时 ${formatDuration(elapsedOf(mark))}`
  }
  return null
}

/** 无进展提示：分析/生成超过 15s 没有新的 chars 增量时提示「仍在等待
 *  模型响应（已 45s）」；有新增量即消失。 */
const stallHint = computed(() => {
  if (!ingesting.value || lastDeltaAt.value === null) return null
  if (phase.value !== 'analyzing' && phase.value !== 'generating') return null
  const stalledFor = now.value - lastDeltaAt.value
  if (stalledFor <= STALL_HINT_THRESHOLD_MS) return null
  return `仍在等待模型响应（已 ${Math.floor(stalledFor / 1000)}s）`
})

async function chooseDocFile(): Promise<void> {
  // 桌面壳：系统对话框（原样不动）。浏览器：plugin-dialog 依赖 Tauri
  // internals（invoke 直接抛错），改开服务器侧文件选择弹窗。
  if (isTauri) {
    // 任意位置的文档：导入 raw/ 由命令端自动完成（复制，源文件不动）
    const picked = await open({
      multiple: false,
      filters: [{ name: '文档（Markdown / 纯文本）', extensions: ['md', 'markdown', 'txt'] }],
    })
    if (typeof picked === 'string') {
      srcPath.value = picked
      ingestError.value = null
    }
    return
  }
  pickerOpen.value = true
}

/** PathPicker 选中（.md / .markdown / .txt 文件）：即待摄取文档路径。 */
function onPicked(path: string): void {
  srcPath.value = path
  ingestError.value = null
  pickerOpen.value = false
}

async function runIngest(): Promise<void> {
  if (!canIngest.value || vault.value === null || srcPath.value === null) return
  ingesting.value = true
  // 进度时间线归零：读取阶段从点击开始计时（后端首事件同 key 自动去重）
  stageMarks.value = []
  llmChars.value = 0
  lastDeltaAt.value = null
  phase.value = 'reading'
  enterStage('reading')
  report.value = null
  ingestError.value = null
  try {
    report.value = await ingestFile(vault.value.path, srcPath.value)
    phase.value = 'done'
    // 落盘成功：Wiki 有新页面，通知文件面板重拉页面树
    if (report.value.applied) bumpPages()
  } catch (err) {
    ingestError.value = errorMessage(err)
    phase.value = 'error'
  } finally {
    closeStages()
    ingesting.value = false
  }
}

function pageTypeLabel(kind: PageKind): string {
  return PAGE_TYPE_LABELS[kind]
}
</script>

<template>
  <main class="page">
    <header class="page-header">
      <h2>摄取（Ingest）</h2>
      <p class="subtitle">从任意位置选文件 → 自动导入 raw/ → 编译进 Wiki：指纹检查 → 两步 LLM → Draft → Apply（自动或人审由配置决定）</p>
      <p v-if="vault" class="subtitle vault-line">
        当前 Vault：<code :title="vault.path">{{ vault.path }}</code>
      </p>
    </header>

    <section class="card step">
      <h3>1 · 选择文档（任意位置）</h3>
      <label class="field-label" for="raw-path">文档（任意位置；摄取时自动复制进当前 Vault 的 raw/，源文件不动）</label>
      <div class="path-row">
        <output id="raw-path" class="path-box" :title="srcPath ?? undefined">
          {{ srcPath ?? '尚未选择文件' }}
        </output>
        <button type="button" class="btn" :disabled="ingesting" @click="chooseDocFile">
          浏览…
        </button>
      </div>
    </section>

    <div class="actions">
      <button
        type="button"
        class="btn primary"
        :disabled="!canIngest"
        @click="runIngest"
      >
        {{ ingesting ? '摄取中…' : '开始摄取' }}
      </button>
    </div>

    <PathPicker v-if="pickerOpen" mode="file" @picked="onPicked" @cancel="pickerOpen = false" />

    <section v-if="ingesting || phase === 'done' || phase === 'error'" class="card step">
      <h3>2 · 进度</h3>
      <ol class="steps">
        <li
          v-for="(step, idx) in STEPS"
          :key="step.key"
          class="step-item"
          :class="{ active: idx === currentStep, done: idx < currentStep }"
        >
          <span class="step-dot" />
          <span class="step-body">
            <span class="step-line">{{ stepTitle(idx) }}</span>
            <span v-if="stepSub(idx) !== null" class="step-sub">{{ stepSub(idx) }}</span>
          </span>
        </li>
      </ol>
      <p v-if="stallHint" class="hint stall" role="status">{{ stallHint }}</p>
      <p v-if="phase === 'done' && report?.skipped" class="hint">
        文件内容未变化（指纹命中），已跳过——零 LLM 调用、零写入。
      </p>
    </section>

    <section v-if="ingestError" class="card result error">
      <h3>摄取失败</h3>
      <p class="error-text">{{ ingestError }}</p>
      <p class="hint">失败不产生半写状态：Wiki 与 git 与摄取前一致。</p>
    </section>

    <section v-if="report && phase === 'done'" class="card result success">
      <h3>{{ report.skipped ? '指纹命中，已跳过' : manualPending ? '摄取完成（Draft 未落盘）' : '摄取完成' }}</h3>
      <dl class="meta">
        <div class="meta-row">
          <dt>导入</dt>
          <dd>{{ report.newly_imported ? `已导入 ${report.imported_raw_path}` : `已在库：${report.imported_raw_path}` }}</dd>
        </div>
        <div class="meta-row">
          <dt>Source</dt>
          <dd>{{ report.source_title }}</dd>
        </div>
        <div class="meta-row">
          <dt>指纹</dt>
          <dd><code class="fingerprint">{{ report.fingerprint.slice(0, 16) }}…</code></dd>
        </div>
        <div v-if="report.commit" class="meta-row">
          <dt>提交</dt>
          <dd><code>{{ report.commit }}</code></dd>
        </div>
      </dl>

      <div v-if="report.note" class="manual-note">
        <p>{{ report.note }}</p>
        <pre v-if="report.summary_lines.length > 0" class="summary-lines">{{ report.summary_lines.join('\n') }}</pre>
      </div>

      <h4 v-if="report.pages.length > 0" class="pages-title">产出页面（{{ report.pages.length }}）</h4>
      <table v-if="report.pages.length > 0" class="pages-table">
        <thead>
          <tr>
            <th>类型</th>
            <th>标题</th>
            <th>动作</th>
            <th>路径</th>
          </tr>
        </thead>
        <tbody>
          <tr v-for="page in report.pages" :key="page.path">
            <td>
              <span class="type-badge" :class="page.page_type">{{ pageTypeLabel(page.page_type) }}</span>
            </td>
            <td>{{ page.title }}</td>
            <td>{{ page.action === 'create' ? '新建' : '更新' }}</td>
            <td><code>{{ page.path }}</code></td>
          </tr>
        </tbody>
      </table>

      <template v-if="report.llm_calls && report.llm_calls.length > 0">
        <h4 class="pages-title">LLM 调用统计（{{ report.llm_calls.length }} 次）</h4>
        <table class="pages-table">
          <thead>
            <tr>
              <th>档位</th>
              <th>用途</th>
              <th>尝试</th>
              <th>输出字数</th>
              <th>耗时</th>
              <th>结果</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="(call, i) in report.llm_calls" :key="i">
              <td>{{ call.tier === 'fast' ? '快速' : '强力' }}</td>
              <td>{{ call.purpose === 'analyze' ? '分析' : '成文' }}</td>
              <td>{{ call.attempt }}</td>
              <td>{{ call.output_chars.toLocaleString() }}</td>
              <td>{{ call.duration_ms >= 1000 ? `${(call.duration_ms / 1000).toFixed(1)}s` : `${call.duration_ms}ms` }}</td>
              <td>
                <span :title="call.error ?? ''">{{
                  call.outcome === 'ok' ? '成功'
                  : call.outcome === 'oversized' ? '超限熔断'
                  : call.outcome === 'parse_failed' ? '解析失败'
                  : '调用错误'
                }}</span>
              </td>
            </tr>
          </tbody>
        </table>
        <p class="hint">完整统计同步记录在 Vault 的 .corpus/llm-calls.log（不含任何 prompt / 输出原文）。</p>
      </template>
    </section>
  </main>
</template>

<style scoped>
.page {
  width: 100%;
  max-width: 760px;
  margin: 0 auto;
  padding: 28px 24px 48px;
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.page-header .subtitle {
  color: var(--text-muted);
  margin-top: 6px;
}

.page-header .vault-line {
  margin-top: 4px;
  font-size: 13px;
}

.page-header .vault-line code {
  font-size: 12px;
}

.step h3 {
  font-size: 15px;
  margin-bottom: 14px;
}

.card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 20px;
}

.field-label {
  display: block;
  margin: 12px 0 6px;
  font-size: 13px;
  color: var(--text-muted);
}

.field-label:first-of-type {
  margin-top: 0;
}

.path-row {
  display: flex;
  gap: 10px;
  align-items: stretch;
}

.path-box {
  flex: 1;
  display: flex;
  align-items: center;
  min-height: 38px;
  padding: 4px 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  font-family: var(--mono);
  font-size: 13px;
  color: var(--text-strong);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.hint {
  margin-top: 10px;
  font-size: 13px;
  color: var(--text-muted);
}

.actions {
  display: flex;
  justify-content: flex-end;
}

.btn {
  padding: 8px 18px;
  border-radius: 6px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text-strong);
  font-size: 14px;
  cursor: pointer;
}

.btn:hover:not(:disabled) {
  border-color: var(--accent-border);
}

.btn:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn.primary {
  background: var(--accent);
  border-color: var(--accent);
  color: #fff;
}

.btn.primary:hover:not(:disabled) {
  filter: brightness(1.08);
}

.steps {
  list-style: none;
  display: flex;
  gap: 8px;
  padding: 0;
  margin: 0;
}

.step-item {
  flex: 1;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: 8px;
  font-size: 13px;
  color: var(--text-muted);
  background: var(--surface);
  min-width: 0;
}

.step-item.active {
  border-color: var(--accent);
  color: var(--accent);
  background: var(--accent-soft);
  font-weight: 600;
}

.step-item.done {
  color: var(--ok);
  border-color: var(--ok-border);
}

.step-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: currentColor;
  flex: none;
}

.step-body {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.step-line {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.step-sub {
  font-size: 11px;
  font-weight: 400;
  opacity: 0.85;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.hint.stall {
  color: var(--text-strong);
  font-size: 12px;
}

.result.error {
  border-color: var(--danger-border);
  background: var(--danger-soft);
}

.result.error h3 {
  color: var(--danger);
  font-size: 15px;
}

.error-text {
  margin-top: 8px;
  font-size: 14px;
  color: var(--danger);
  word-break: break-all;
}

.result.success {
  border-color: var(--ok-border);
  background: var(--ok-soft);
}

.result.success h3 {
  color: var(--ok);
  font-size: 15px;
}

.meta {
  margin: 12px 0 0;
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.meta-row {
  display: flex;
  gap: 12px;
  font-size: 14px;
}

.meta-row dt {
  width: 48px;
  flex: none;
  color: var(--text-muted);
}

.meta-row dd {
  margin: 0;
  color: var(--text-strong);
  word-break: break-all;
}

.fingerprint {
  font-family: var(--mono);
  font-size: 12px;
}

.manual-note {
  margin-top: 14px;
  padding: 12px 14px;
  border: 1px dashed var(--accent-border);
  border-radius: 8px;
  background: var(--accent-soft);
}

.manual-note p {
  font-size: 13px;
  color: var(--text-strong);
  margin: 0;
}

.summary-lines {
  margin: 10px 0 0;
  padding: 10px 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  font-family: var(--mono);
  font-size: 12px;
  color: var(--text-muted);
  white-space: pre-wrap;
  word-break: break-all;
}

.pages-title {
  margin: 18px 0 8px;
  font-size: 14px;
  color: var(--text-strong);
}

.pages-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 13px;
  background: var(--surface);
  border-radius: 8px;
  overflow: hidden;
}

.pages-table th,
.pages-table td {
  text-align: left;
  padding: 8px 10px;
  border-bottom: 1px solid var(--border);
}

.pages-table th {
  color: var(--text-muted);
  font-weight: 600;
  background: var(--accent-soft);
}

.pages-table td code {
  font-family: var(--mono);
  font-size: 12px;
}

.type-badge {
  display: inline-block;
  padding: 2px 8px;
  border-radius: 999px;
  font-size: 12px;
  border: 1px solid var(--border);
  color: var(--text-muted);
}

.type-badge.source {
  color: var(--accent);
  border-color: var(--accent-border);
  background: var(--accent-soft);
}
</style>
