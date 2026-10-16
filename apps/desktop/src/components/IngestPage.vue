<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import {
  ingestFile,
  onIngestProgress,
  type IngestProgress,
  type IngestReport,
  type PageKind,
} from '../api'
import { errorMessage } from '../display'
import { bumpPages, currentVault } from '../vault-state'

/** 四阶段步骤条（引擎 IngestStage 的 UI 映射）。 */
const STEPS: ReadonlyArray<{ key: IngestProgress['stage']; label: string }> = [
  { key: 'reading', label: '读取' },
  { key: 'analyzing', label: '分析' },
  { key: 'generating', label: '生成' },
  { key: 'applying', label: '落盘' },
]

const PAGE_TYPE_LABELS: Record<PageKind, string> = {
  source: 'Source 页',
  entity: 'Entity',
  concept: 'Concept',
}

type Phase = 'idle' | IngestProgress['stage'] | 'done' | 'error'

const rawPath = ref<string | null>(null)
const ingesting = ref(false)
const phase = ref<Phase>('idle')
const report = ref<IngestReport | null>(null)
const ingestError = ref<string | null>(null)

let unlisten: (() => void) | null = null

onMounted(async () => {
  // 进度事件：命令执行中由后端 emit；卸载必须 unlisten（调研笔记第 4 节）
  unlisten = await onIngestProgress((progress) => {
    if (ingesting.value) phase.value = progress.stage
  })
})

onUnmounted(() => {
  unlisten?.()
  unlisten = null
})

/** 摄取只作用于当前打开的 Vault（工作区面板；无 Vault 时不会挂载）。 */
const vault = computed(() => currentVault.value)

const canIngest = computed(
  () => vault.value !== null && rawPath.value !== null && !ingesting.value,
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

async function chooseRawFile(): Promise<void> {
  const picked = await open({
    multiple: false,
    filters: [{ name: '文档（Markdown / 纯文本）', extensions: ['md', 'markdown', 'txt'] }],
  })
  if (typeof picked === 'string') {
    rawPath.value = picked
    ingestError.value = null
  }
}

async function runIngest(): Promise<void> {
  if (!canIngest.value || vault.value === null || rawPath.value === null) return
  ingesting.value = true
  phase.value = 'reading'
  report.value = null
  ingestError.value = null
  try {
    report.value = await ingestFile(vault.value.path, rawPath.value)
    phase.value = 'done'
    // 落盘成功：Wiki 有新页面，通知文件面板重拉页面树
    if (report.value.applied) bumpPages()
  } catch (err) {
    ingestError.value = errorMessage(err)
    phase.value = 'error'
  } finally {
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
      <p class="subtitle">把 raw/ 中的文档编译进 Wiki：指纹检查 → 两步 LLM → Draft → Apply（自动或人审由配置决定）</p>
      <p v-if="vault" class="subtitle vault-line">
        当前 Vault：<code :title="vault.path">{{ vault.path }}</code>
      </p>
    </header>

    <section class="card step">
      <h3>1 · 选择 raw 文件</h3>
      <label class="field-label" for="raw-path">raw 文件（须位于当前 Vault 的 raw/ 目录内）</label>
      <div class="path-row">
        <output id="raw-path" class="path-box" :title="rawPath ?? undefined">
          {{ rawPath ?? '尚未选择文件' }}
        </output>
        <button type="button" class="btn" :disabled="ingesting" @click="chooseRawFile">
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
          <span>{{ step.label }}</span>
        </li>
      </ol>
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
