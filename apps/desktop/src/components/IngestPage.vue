<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import {
  ingestFile,
  listRecentVaults,
  onIngestProgress,
  type IngestProgress,
  type IngestReport,
  type PageKind,
  type RecentVault,
} from '../api'
import { errorMessage, templateDisplayName } from '../display'
import { currentVault } from '../vault-state'

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

const recentVaults = ref<RecentVault[]>([])
const vaultsError = ref<string | null>(null)
const selectedVaultPath = ref<string | null>(null)
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
  try {
    recentVaults.value = await listRecentVaults()
  } catch (err) {
    vaultsError.value = errorMessage(err)
  }
  if (currentVault.value) selectedVaultPath.value = currentVault.value.path
})

onUnmounted(() => {
  unlisten?.()
  unlisten = null
})

const canIngest = computed(
  () => selectedVaultPath.value !== null && rawPath.value !== null && !ingesting.value,
)

/** 当前步骤索引（done 时全部完成，idle 时 -1）。 */
const currentStep = computed(() => {
  if (phase.value === 'done') return STEPS.length
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
  if (!canIngest.value || selectedVaultPath.value === null || rawPath.value === null) return
  ingesting.value = true
  phase.value = 'reading'
  report.value = null
  ingestError.value = null
  try {
    report.value = await ingestFile(selectedVaultPath.value, rawPath.value)
    phase.value = 'done'
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
      <p class="subtitle">把 raw/ 中的文档编译进 Wiki：指纹检查 → 两步 LLM → Draft → 自动 Apply</p>
    </header>

    <section class="card step">
      <h3>1 · 选择 Vault 与 raw 文件</h3>
      <div v-if="vaultsError" class="load-error">
        <p>Vault 列表加载失败：{{ vaultsError }}</p>
      </div>
      <div v-else-if="recentVaults.length === 0" class="hint">
        尚无最近打开的 Vault——请先在「创建 Vault」页创建或到设置页登记已有 Vault。
      </div>
      <template v-else>
        <label class="field-label" for="vault-select">Vault</label>
        <select id="vault-select" v-model="selectedVaultPath" class="select" :disabled="ingesting">
          <option v-for="v in recentVaults" :key="v.path" :value="v.path">
            {{ v.path }}（{{ templateDisplayName(v.template_id) }}）
          </option>
        </select>

        <label class="field-label" for="raw-path">raw 文件（须位于所选 Vault 的 raw/ 目录内）</label>
        <div class="path-row">
          <output id="raw-path" class="path-box" :title="rawPath ?? undefined">
            {{ rawPath ?? '尚未选择文件' }}
          </output>
          <button type="button" class="btn" :disabled="ingesting" @click="chooseRawFile">
            浏览…
          </button>
        </div>
      </template>
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
      <h3>{{ report.skipped ? '指纹命中，已跳过' : '摄取完成' }}</h3>
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
  padding: 40px 24px 64px;
  display: flex;
  flex-direction: column;
  gap: 20px;
}

.page-header .subtitle {
  color: var(--text-muted);
  margin-top: 6px;
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

.select {
  width: 100%;
  min-height: 38px;
  padding: 4px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text-strong);
  font-size: 13px;
  font-family: var(--mono);
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

.load-error p {
  font-size: 14px;
  color: var(--danger);
  word-break: break-all;
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
