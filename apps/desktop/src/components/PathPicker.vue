<script setup lang="ts">
/**
 * 服务器侧路径选择弹窗（web 壳专用）。
 *
 * 浏览器弹不了系统文件对话框（plugin-dialog 依赖 Tauri internals），
 * 改用本组件：经 listFs 只读列目录（server 仅允许用户主目录之内），
 * 点目录进入、上级目录回退。挂载即列 home（home 路径从首次响应的
 * path 字段获得，前端不自己猜）。仅 web 模式下由页面挂载（Tauri 下
 * 页面走 plugin-dialog，不渲染本组件）。
 */
import { onMounted, ref } from 'vue'
import { listFs, type FsEntry } from '../api'
import { errorMessage } from '../display'

const props = defineProps<{
  /** file = 选文档（仅 .md/.markdown/.txt 可选中）；dir = 选目录。 */
  mode: 'file' | 'dir'
}>()

const emit = defineEmits<{
  (e: 'picked', path: string): void
  (e: 'cancel'): void
}>()

/** file 模式允许选中的扩展名（与桌面壳 open() filters 同一口径）。 */
const ALLOWED_DOC_EXTS = ['.md', '.markdown', '.txt']

const currentPath = ref('')
const homePath = ref('')
const entries = ref<FsEntry[]>([])
const loading = ref(true)
const loadError = ref<string | null>(null)
/** dir 模式的「新目录名」：填写后选择 = 当前目录/新名（如建新 Vault）。 */
const newDirName = ref('')

function isAllowedFile(entry: FsEntry): boolean {
  const lower = entry.name.toLowerCase()
  return ALLOWED_DOC_EXTS.some((ext) => lower.endsWith(ext))
}

/** 进入某目录列其条目；path 为空 = home（缺省语义由 server 承担）。 */
async function enter(path: string): Promise<void> {
  loading.value = true
  loadError.value = null
  try {
    const listing = await listFs(path === '' ? undefined : path)
    currentPath.value = listing.path
    if (homePath.value === '') homePath.value = listing.path
    entries.value = listing.entries
  } catch (err) {
    // 失败保留当前目录（可重试 / 可继续浏览），错误内联展示
    loadError.value = errorMessage(err)
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  void enter('')
})

function openEntry(entry: FsEntry): void {
  if (entry.is_dir) {
    void enter(entry.path)
    return
  }
  if (props.mode === 'file' && isAllowedFile(entry)) {
    emit('picked', entry.path)
  }
}

/** 上级目录：已在家目录（或更上层不可达，server 只放行 home 之内）
 *  时不动，避免请求注定 403 的路径。 */
function goUp(): void {
  const cur = currentPath.value
  if (cur === '' || cur === homePath.value) return
  const trimmed = cur.replace(/\/+$/, '')
  const idx = trimmed.lastIndexOf('/')
  if (idx <= 0) return
  void enter(trimmed.slice(0, idx))
}

function pickNewSubdir(): void {
  const name = newDirName.value.trim()
  if (name === '' || currentPath.value === '') return
  const sep = currentPath.value.endsWith('/') ? '' : '/'
  emit('picked', `${currentPath.value}${sep}${name}`)
}

function pickCurrent(): void {
  if (currentPath.value !== '') emit('picked', currentPath.value)
}
</script>

<template>
  <div class="picker-overlay" @click.self="emit('cancel')">
    <div class="picker-dialog" role="dialog" aria-modal="true" :aria-label="mode === 'dir' ? '选择目录' : '选择文件'">
      <header class="picker-header">
        <h3>{{ mode === 'dir' ? '选择目录' : '选择文件' }}</h3>
        <button type="button" class="btn btn-sm" @click="emit('cancel')">取消</button>
      </header>

      <p class="picker-path">
        <span class="picker-path-label">当前</span>
        <code :title="currentPath">{{ currentPath !== '' ? currentPath : '加载中…' }}</code>
      </p>

      <div class="picker-toolbar">
        <button
          type="button"
          class="btn btn-sm"
          :disabled="loading || currentPath === '' || currentPath === homePath"
          @click="goUp"
        >
          上级目录
        </button>
        <span v-if="mode === 'dir'" class="toolbar-hint">点目录名进入；隐藏文件不显示</span>
        <span v-else class="toolbar-hint">仅 .md / .markdown / .txt 可选中</span>
      </div>

      <div v-if="loading" class="picker-state">正在加载…</div>
      <div v-else-if="loadError !== null" class="picker-state picker-error">
        <p>{{ loadError }}</p>
        <button type="button" class="btn btn-sm" @click="enter(currentPath)">重试</button>
      </div>
      <ul v-else-if="entries.length > 0" class="picker-list">
        <li v-for="entry in entries" :key="entry.path">
          <button
            type="button"
            class="picker-entry"
            :class="{ 'is-dir': entry.is_dir, dim: mode === 'file' && !entry.is_dir && !isAllowedFile(entry) }"
            :disabled="mode === 'file' && !entry.is_dir && !isAllowedFile(entry)"
            :title="entry.path"
            @click="openEntry(entry)"
          >
            <span class="entry-name">{{ entry.name }}</span>
            <span v-if="entry.is_dir" class="entry-kind">目录</span>
          </button>
        </li>
      </ul>
      <div v-else class="picker-state">此目录为空</div>

      <footer v-if="mode === 'dir'" class="picker-footer">
        <div class="newdir-row">
          <label class="newdir-label" for="picker-new-dir">新目录名</label>
          <input
            id="picker-new-dir"
            v-model="newDirName"
            type="text"
            placeholder="填写则选择 = 当前目录 / 新目录名（可新建 Vault）"
            @keyup.enter="pickNewSubdir"
          />
          <button
            type="button"
            class="btn btn-sm"
            :disabled="newDirName.trim() === '' || currentPath === ''"
            @click="pickNewSubdir"
          >
            选择
          </button>
        </div>
        <button type="button" class="btn btn-sm primary" :disabled="currentPath === ''" @click="pickCurrent">
          选择当前目录
        </button>
      </footer>
    </div>
  </div>
</template>

<style scoped>
.picker-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
  padding: 24px;
}

.picker-dialog {
  width: min(640px, 100%);
  max-height: min(560px, 90vh);
  display: flex;
  flex-direction: column;
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 18px;
  gap: 12px;
  box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25);
}

.picker-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.picker-header h3 {
  font-size: 15px;
}

.picker-path {
  margin: 0;
  display: flex;
  align-items: baseline;
  gap: 8px;
  min-width: 0;
}

.picker-path-label {
  flex: none;
  font-size: 12px;
  color: var(--text-muted);
}

.picker-path code {
  font-family: var(--mono);
  font-size: 12px;
  color: var(--text-strong);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.picker-toolbar {
  display: flex;
  align-items: center;
  gap: 10px;
}

.toolbar-hint {
  font-size: 12px;
  color: var(--text-muted);
}

.picker-list {
  list-style: none;
  margin: 0;
  padding: 0;
  overflow-y: auto;
  border: 1px solid var(--border);
  border-radius: 8px;
  min-height: 160px;
}

.picker-entry {
  width: 100%;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 10px;
  padding: 8px 12px;
  border: none;
  border-bottom: 1px solid var(--border);
  background: transparent;
  color: var(--text-strong);
  font-size: 13px;
  text-align: left;
  cursor: pointer;
}

.picker-list li:last-child .picker-entry {
  border-bottom: none;
}

.picker-entry:hover:not(:disabled) {
  background: var(--accent-soft);
}

.picker-entry:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}

.picker-entry.is-dir .entry-name {
  font-weight: 600;
}

.picker-entry.dim,
.picker-entry:disabled {
  color: var(--text-muted);
  cursor: not-allowed;
  opacity: 0.6;
}

.entry-kind {
  flex: none;
  font-size: 11px;
  color: var(--text-muted);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 1px 8px;
}

.entry-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.picker-state {
  min-height: 160px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 10px;
  border: 1px dashed var(--border);
  border-radius: 8px;
  font-size: 13px;
  color: var(--text-muted);
}

.picker-error p {
  margin: 0;
  color: var(--danger);
  word-break: break-all;
  text-align: center;
  max-width: 90%;
}

.picker-footer {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  flex-wrap: wrap;
}

.newdir-row {
  display: flex;
  align-items: center;
  gap: 8px;
  flex: 1;
  min-width: 260px;
}

.newdir-label {
  flex: none;
  font-size: 13px;
  color: var(--text-muted);
}

.newdir-row input {
  flex: 1;
  min-width: 0;
  padding: 7px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  font-size: 13px;
  color: var(--text-strong);
  background: var(--surface);
}

.newdir-row input:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
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

.btn.btn-sm {
  padding: 6px 12px;
  font-size: 13px;
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
</style>
