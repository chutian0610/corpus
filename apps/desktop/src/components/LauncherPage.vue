<script setup lang="ts">
/** 启动页（一级界面，currentVault 为空时全屏）：
 *  最近 Vault 列表 + 打开其他 Vault + 新建 Vault。
 *  「打开」统一走 vault-state.openVault（引擎校验 + 最近列表登记），
 *  非 Vault 目录在这里内联报错。 */
import { onMounted, ref } from 'vue'
import { open as openFileDialog } from '@tauri-apps/plugin-dialog'
import {
  listRecentVaults,
  removeRecentVault,
  type RecentVault,
} from '../api'
import { errorMessage, formatDateTime, pathBasename, templateDisplayName } from '../display'
import { openVault } from '../vault-state'
import VaultCreatePage from './VaultCreatePage.vue'

/** 启动恢复失败的上报（App 传入；用户上次的 Vault 打不开时提示）。 */
const props = defineProps<{ startupError?: string | null }>()

type Mode = 'home' | 'create'

const mode = ref<Mode>('home')
const recentVaults = ref<RecentVault[]>([])
const listError = ref<string | null>(null)
const loading = ref(true)
/** 「打开」类操作的内联报错（对话框选中的目录不是 Vault 等）。 */
const openError = ref<string | null>(null)
const openingPath = ref<string | null>(null)
const removingPath = ref<string | null>(null)

onMounted(loadRecent)

async function loadRecent(): Promise<void> {
  loading.value = true
  listError.value = null
  try {
    recentVaults.value = await listRecentVaults()
  } catch (err) {
    listError.value = errorMessage(err)
  } finally {
    loading.value = false
  }
}

/** 打开最近列表（或对话框选中）的 Vault；失败内联报错，停留在启动页。 */
async function tryOpen(path: string): Promise<void> {
  openingPath.value = path
  openError.value = null
  try {
    await openVault(path)
    // 成功：currentVault 置位，App 切到工作区，本页随之卸载
  } catch (err) {
    openError.value = errorMessage(err)
  } finally {
    openingPath.value = null
  }
}

/** 打开其他 Vault：目录对话框 → open_vault 校验（非 Vault 明确报错）。 */
async function openOther(): Promise<void> {
  const picked = await openFileDialog({ directory: true, multiple: false })
  if (typeof picked === 'string') await tryOpen(picked)
}

async function handleRemove(path: string): Promise<void> {
  removingPath.value = path
  openError.value = null
  try {
    await removeRecentVault(path)
    recentVaults.value = recentVaults.value.filter((v) => v.path !== path)
  } catch (err) {
    openError.value = errorMessage(err)
  } finally {
    removingPath.value = null
  }
}

/** 新建成功：立即打开进入工作区（失败内联报错，停留创建页）。 */
async function onCreated(root: string): Promise<void> {
  await tryOpen(root)
}
</script>

<template>
  <main class="launcher">
    <div class="launcher-inner">
      <header class="brand-block">
        <h1 class="brand">Corpus</h1>
        <p class="tagline">LLM 增量维护的个人知识库：一个目录即一个 Vault</p>
      </header>

      <template v-if="mode === 'home'">
        <p v-if="props.startupError" class="startup-error">
          上次打开的 Vault 恢复失败，已回到启动页：{{ props.startupError }}
        </p>

        <section class="card">
          <div class="section-head">
            <h2>最近打开</h2>
            <span class="hint-inline">点条目或「打开」进入工作区；移除只动列表，不删文件</span>
          </div>

          <div v-if="loading" class="hint">正在加载…</div>
          <p v-else-if="listError" class="error-text">最近列表加载失败：{{ listError }}</p>
          <div v-else-if="recentVaults.length === 0" class="hint empty">
            还没有打开过的 Vault——在下方新建一个，或打开磁盘上的已有 Vault。
          </div>
          <ul v-else class="vault-list">
            <li v-for="v in recentVaults" :key="v.path">
              <button
                type="button"
                class="vault-row"
                :disabled="openingPath === v.path || removingPath === v.path"
                :title="`打开 ${v.path}`"
                @click="tryOpen(v.path)"
              >
                <span class="vault-name">{{ pathBasename(v.path) }}</span>
                <span class="vault-meta">
                  {{ templateDisplayName(v.template_id) }} · {{ formatDateTime(v.opened_at) }}
                </span>
                <code class="vault-path">{{ v.path }}</code>
              </button>
              <button
                type="button"
                class="btn small"
                :disabled="openingPath === v.path || removingPath === v.path"
                @click="tryOpen(v.path)"
              >
                {{ openingPath === v.path ? '打开中…' : '打开' }}
              </button>
              <button
                type="button"
                class="btn small ghost"
                :disabled="removingPath === v.path || openingPath === v.path"
                :title="`从最近列表移除（不删除文件）`"
                @click="handleRemove(v.path)"
              >
                {{ removingPath === v.path ? '移除中…' : '移除' }}
              </button>
            </li>
          </ul>
        </section>

        <p v-if="openError" class="error-text card error-box">{{ openError }}</p>

        <div class="actions">
          <button type="button" class="btn" @click="openOther">打开其他 Vault…</button>
          <button type="button" class="btn primary" @click="mode = 'create'">+ 新建 Vault…</button>
        </div>
      </template>

      <template v-else>
        <div class="create-head">
          <button type="button" class="btn small" @click="mode = 'home'">← 返回</button>
          <span class="hint-inline">新建成功后自动打开，直接进入工作区</span>
        </div>
        <VaultCreatePage @created="onCreated" />
      </template>
    </div>
  </main>
</template>

<style scoped>
.launcher {
  min-height: 100vh;
  display: flex;
  justify-content: center;
  padding: 48px 24px;
  overflow-y: auto;
}

.launcher-inner {
  width: 100%;
  max-width: 640px;
  display: flex;
  flex-direction: column;
  gap: 18px;
}

.brand-block {
  text-align: center;
  margin-bottom: 10px;
}

.brand {
  font-size: 34px;
  letter-spacing: 1px;
}

.tagline {
  margin-top: 8px;
  color: var(--text-muted);
  font-size: 14px;
}

.card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 18px 20px;
}

.card h2 {
  font-size: 15px;
}

.section-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  flex-wrap: wrap;
  margin-bottom: 12px;
}

.hint-inline {
  font-size: 12px;
  color: var(--text-muted);
}

.hint {
  font-size: 13px;
  color: var(--text-muted);
}

.hint.empty {
  padding: 18px 0;
  text-align: center;
}

.startup-error {
  font-size: 13px;
  color: var(--danger);
  padding: 10px 14px;
  border: 1px solid var(--danger-border);
  border-radius: 8px;
  background: var(--danger-soft);
  word-break: break-all;
}

.error-text {
  font-size: 13px;
  color: var(--danger);
  word-break: break-all;
}

.error-box {
  border-color: var(--danger-border);
  background: var(--danger-soft);
}

.vault-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.vault-list li {
  display: flex;
  align-items: stretch;
  gap: 8px;
}

.vault-row {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 2px;
  padding: 10px 14px;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface);
  text-align: left;
  cursor: pointer;
}

.vault-row:hover:not(:disabled) {
  border-color: var(--accent-border);
  background: var(--accent-soft);
}

.vault-row:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

.vault-row:disabled {
  opacity: 0.6;
  cursor: wait;
}

.vault-name {
  color: var(--text-strong);
  font-weight: 600;
  font-size: 14px;
}

.vault-meta {
  font-size: 12px;
  color: var(--text-muted);
}

.vault-path {
  font-size: 12px;
  color: var(--text-muted);
  max-width: 100%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  background: transparent;
  padding: 0;
}

.actions {
  display: flex;
  justify-content: center;
  gap: 12px;
}

.create-head {
  display: flex;
  align-items: center;
  gap: 12px;
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

.btn.small {
  padding: 4px 12px;
  font-size: 13px;
}

.btn.ghost {
  color: var(--text-muted);
}
</style>
