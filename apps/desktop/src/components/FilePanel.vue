<script setup lang="ts">
/** 工作区左文件面板：当前 Vault 的 wiki/ 页面树，按页面类型分组折叠。
 *  数据来自共享的 vault-state pageTree（list_pages 纯文件系统遍历；
 *  PagePreview 的 wikilink 解析共用同一份）；Apply 落盘后 pagesVersion
 *  bump，本面板据此自动重拉。点条目 → 主区页面渲染（T4）。
 *  T4 新增：顶部固定「目录」入口（wiki/index.md，页面树不含引擎自维护
 *  文件，read_page_typed 可读）与文本过滤框（按路径 / title / 别名实时
 *  过滤，空串显示全部，分组保持）。 */
import { computed, onMounted, ref, watch } from 'vue'
import type { PageEntry, PageGroup } from '../api'
import { PAGE_TYPE_LABELS, pageDisplayName } from '../display'
import { currentVault, pageTree, pageTreeError, pageTreeLoading, pagesVersion, reloadPageTree } from '../vault-state'

const props = defineProps<{
  /** 当前选中的页面路径（高亮；null = 未选中）。 */
  selected: string | null
}>()

const emit = defineEmits<{
  (e: 'select', page: PageEntry): void
}>()

/** 各分组折叠态（key = kind；默认展开）。 */
const collapsed = ref<Record<string, boolean>>({})
/** 文本过滤（T4）：匹配路径 / frontmatter title / 别名，大小写不敏感。 */
const filter = ref('')

/** 目录入口条目（wiki/index.md 不在 PageKind 分组内，作为固定首项）。 */
const INDEX_ENTRY: PageEntry = { path: 'index.md', updated: null, title: 'Wiki Index', aliases: [] }

/** 过滤后的分组：查询为空串时全量透传；非空时按路径 / title / 别名
 *  包含匹配（不区分大小写），滤空的分组不显示。 */
const filteredGroups = computed<PageGroup[]>(() => {
  const query = filter.value.trim().toLowerCase()
  const groups = pageTree.value?.groups ?? []
  if (query === '') return groups
  return groups
    .map((g) => ({
      ...g,
      pages: g.pages.filter(
        (p) =>
          p.path.toLowerCase().includes(query) ||
          (p.title ?? '').toLowerCase().includes(query) ||
          p.aliases.some((a) => a.toLowerCase().includes(query)),
      ),
    }))
    .filter((g) => g.pages.length > 0)
})

/** 目录入口是否被过滤隐藏（查询非空且 index.md 各字段都不匹配）。 */
const indexVisible = computed<boolean>(() => {
  const query = filter.value.trim().toLowerCase()
  if (query === '') return true
  return (
    INDEX_ENTRY.path.includes(query) ||
    (INDEX_ENTRY.title ?? '').toLowerCase().includes(query) ||
    INDEX_ENTRY.aliases.some((a) => a.toLowerCase().includes(query))
  )
})

function toggleGroup(kind: string): void {
  collapsed.value[kind] = !collapsed.value[kind]
}

/** 折叠全部分组（工具行按钮）。 */
function collapseAll(): void {
  collapsed.value = Object.fromEntries(
    filteredGroups.value.map((g) => [g.kind as string, true]),
  )
}

onMounted(reloadPageTree)
// 切换 Vault（路径变化）或 Apply 落盘（版本号 bump）→ 重拉页面树
watch(
  () => [currentVault.value?.path, pagesVersion.value] as const,
  () => {
    void reloadPageTree()
  },
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
        :disabled="pageTreeLoading"
        @click="reloadPageTree"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M20 11a8 8 0 1 0-2.3 6.3" />
          <path d="M20 4v7h-7" />
        </svg>
      </button>
    </div>

    <div class="panel-filter">
      <input
        v-model="filter"
        type="search"
        class="filter-input"
        placeholder="过滤页面（路径 / 标题 / 别名）"
        aria-label="过滤页面"
      />
    </div>

    <div class="panel-body">
      <div v-if="pageTreeLoading && !pageTree" class="hint">正在读取页面树…</div>
      <p v-else-if="pageTreeError" class="error-text">{{ pageTreeError }}</p>
      <template v-else-if="(pageTree?.groups.length ?? 0) > 0">
        <button
          v-if="indexVisible"
          type="button"
          class="page-item index-entry"
          :class="{ selected: props.selected === INDEX_ENTRY.path }"
          title="wiki/index.md"
          @click="emit('select', INDEX_ENTRY)"
        >
          <span class="page-name">目录（index）</span>
        </button>

        <section v-for="group in filteredGroups" :key="group.kind" class="group">
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
                <span class="page-name">{{ page.title ?? pageDisplayName(page.path) }}</span>
              </button>
            </li>
          </ul>
        </section>

        <p v-if="filteredGroups.length === 0" class="hint empty">无匹配页面。</p>
      </template>
      <div v-else class="hint empty">Wiki 暂无页面——先摄取一个文档。</div>
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

.panel-filter {
  padding: 8px 10px 4px;
}

.filter-input {
  width: 100%;
  padding: 5px 9px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--bg);
  color: var(--text);
  font-size: 12px;
}

.filter-input:focus {
  outline: 2px solid var(--accent);
  outline-offset: -1px;
  border-color: var(--accent);
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

.index-entry {
  margin: 0 0 6px;
  padding: 5px 8px;
  font-weight: 600;
  color: var(--text-muted);
}

.index-entry:hover {
  color: var(--text-strong);
}

.index-entry.selected {
  color: var(--accent);
}

.page-name {
  overflow: hidden;
  text-overflow: ellipsis;
}
</style>
