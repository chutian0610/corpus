<script setup lang="ts">
/** 工作区左文件面板：当前 Vault 的 wiki/ 页面树，按页面类型分组折叠。
 *  数据来自引擎 list_pages（纯文件系统遍历）；Apply 落盘后
 *  pagesVersion bump，本面板据此自动重拉。点条目 → 主区只读预览。 */
import { onMounted, ref, watch } from 'vue'
import { listPages, type PageEntry, type PageGroup } from '../api'
import { errorMessage, PAGE_TYPE_LABELS, pageDisplayName } from '../display'
import { currentVault, pagesVersion } from '../vault-state'

const props = defineProps<{
  /** 当前选中的页面路径（高亮；null = 未选中）。 */
  selected: string | null
}>()

const emit = defineEmits<{
  (e: 'select', page: PageEntry): void
}>()

const groups = ref<PageGroup[]>([])
const loading = ref(false)
const loadError = ref<string | null>(null)
/** 各分组折叠态（key = kind；默认展开）。 */
const collapsed = ref<Record<string, boolean>>({})

async function load(): Promise<void> {
  const vault = currentVault.value
  if (!vault) return
  loading.value = true
  loadError.value = null
  try {
    const tree = await listPages(vault.path)
    groups.value = tree.groups
  } catch (err) {
    loadError.value = errorMessage(err)
    groups.value = []
  } finally {
    loading.value = false
  }
}

function toggleGroup(kind: string): void {
  collapsed.value[kind] = !collapsed.value[kind]
}

/** 折叠全部分组（工具行按钮）。 */
function collapseAll(): void {
  collapsed.value = Object.fromEntries(
    groups.value.map((g) => [g.kind as string, true]),
  )
}

onMounted(load)
// 切换 Vault（路径变化）或 Apply 落盘（版本号 bump）→ 重拉页面树
watch(
  () => [currentVault.value?.path, pagesVersion.value] as const,
  load,
)

function label(group: PageGroup): string {
  return PAGE_TYPE_LABELS[group.kind] ?? group.kind
}
</script>

<template>
  <aside class="file-panel" aria-label="Wiki 页面树">
    <div class="panel-toolbar">
      <span class="panel-title">页面</span>
      <span class="toolbar-spacer" />
      <button
        type="button"
        class="tool-btn"
        title="折叠全部分组"
        aria-label="折叠全部分组"
        @click="collapseAll"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="m4 9 4-4M4 9l4 4" />
          <path d="M4 9h16" />
          <path d="M20 15H8" />
          <path d="m14 12 3 3-3 3" />
        </svg>
      </button>
      <button
        type="button"
        class="tool-btn"
        title="刷新（重新读取页面树）"
        aria-label="刷新页面树"
        :disabled="loading"
        @click="load"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M20 11a8 8 0 1 0-2.3 6.3" />
          <path d="M20 4v7h-7" />
        </svg>
      </button>
    </div>

    <div class="panel-body">
      <div v-if="loading && groups.length === 0" class="hint">正在读取页面树…</div>
      <p v-else-if="loadError" class="error-text">{{ loadError }}</p>
      <div v-else-if="groups.length === 0" class="hint empty">
        Wiki 暂无页面——先在摄取面板摄取一个文档。
      </div>

      <section v-for="group in groups" :key="group.kind" class="group">
        <button
          type="button"
          class="group-head"
          :aria-expanded="!collapsed[group.kind]"
          @click="toggleGroup(group.kind)"
        >
          <svg
            class="chevron"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            stroke-width="2"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="m9 6 6 6-6 6" />
          </svg>
          <span>{{ label(group) }}</span>
          <span class="count">{{ group.pages.length }}</span>
        </button>
        <ul v-show="!collapsed[group.kind]" class="page-list">
          <li v-for="page in group.pages" :key="page.path">
            <button
              type="button"
              class="page-item"
              :class="{ selected: props.selected === page.path }"
              :title="page.path"
              @click="emit('select', page)"
            >
              <span class="page-name">{{ pageDisplayName(page.path) }}</span>
            </button>
          </li>
        </ul>
      </section>
    </div>
  </aside>
</template>

<style scoped>
.file-panel {
  display: flex;
  flex-direction: column;
  min-height: 0;
  background: var(--surface);
  border-right: 1px solid var(--border);
}

.panel-toolbar {
  display: flex;
  align-items: center;
  gap: 4px;
  padding: 8px 10px;
  border-bottom: 1px solid var(--border);
}

.panel-title {
  font-size: 13px;
  font-weight: 600;
  color: var(--text-muted);
  letter-spacing: 0.5px;
}

.toolbar-spacer {
  flex: 1;
}

.tool-btn {
  width: 26px;
  height: 26px;
  display: grid;
  place-items: center;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
}

.tool-btn svg {
  width: 15px;
  height: 15px;
}

.tool-btn:hover:not(:disabled) {
  background: var(--accent-soft);
  color: var(--text-strong);
}

.tool-btn:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.tool-btn:disabled {
  opacity: 0.4;
  cursor: not-allowed;
}

.panel-body {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding: 8px 6px 16px;
}

.hint {
  padding: 10px 8px;
  font-size: 12px;
  color: var(--text-muted);
}

.hint.empty {
  text-align: center;
  padding: 24px 8px;
}

.error-text {
  margin: 8px;
  font-size: 12px;
  color: var(--danger);
  word-break: break-all;
}

.group {
  margin-bottom: 4px;
}

.group-head {
  display: flex;
  align-items: center;
  gap: 4px;
  width: 100%;
  padding: 5px 8px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-muted);
  font-size: 12px;
  font-weight: 600;
  letter-spacing: 0.4px;
  cursor: pointer;
}

.group-head:hover {
  color: var(--text-strong);
  background: var(--accent-soft);
}

.chevron {
  width: 12px;
  height: 12px;
  transition: transform 0.12s ease;
  flex: none;
}

.group-head[aria-expanded='false'] .chevron {
  transform: rotate(0deg);
}

.group-head[aria-expanded='true'] .chevron {
  transform: rotate(90deg);
}

.count {
  margin-left: auto;
  font-family: var(--mono);
  font-size: 11px;
  font-weight: 400;
  color: var(--text-muted);
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 999px;
  padding: 0 6px;
}

.page-list {
  list-style: none;
  margin: 0;
  padding: 0 0 0 14px;
}

.page-item {
  display: block;
  width: 100%;
  padding: 4px 8px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text);
  font-size: 13px;
  text-align: left;
  cursor: pointer;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.page-item:hover {
  background: var(--accent-soft);
  color: var(--text-strong);
}

.page-item:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: -2px;
}

.page-item.selected {
  background: var(--accent-soft);
  color: var(--accent);
  font-weight: 600;
}

.page-name {
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
