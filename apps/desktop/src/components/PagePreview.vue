<script setup lang="ts">
/** 主区页面渲染（票据 #5 T4）：Markdown 正文（代码块 / 表格）+
 *  frontmatter 元信息区 + wikilink 应用内跳转 + 返回导航。
 *
 *  安全：wikilink 由 markdown-it inline rule 生成受控 <a>（属性与文本
 *  均经渲染器转义），实例 html:false——源文本手写 HTML 一律字面展示，
 *  v-html 内容不含任何未转义的用户文本（见 wiki-links.ts 安全模型）。
 *  wikilink 点击在容器上委托：解析命中 → navigate；死链 → 红色虚线样式
 *  + 提示条「页面不存在」，不崩溃、不跳转。 */
import { computed, ref, watch } from 'vue'
import { readPage } from '../api'
import { errorMessage, formatDateTime, pageDisplayName, PAGE_TYPE_LABELS } from '../display'
import { currentVault, pageTree } from '../vault-state'
import { buildResolver, parseMetaLink, renderWikiMarkdown } from '../wiki-links'

const props = defineProps<{
  /** 相对 wiki/ 的页面路径（FilePanel 选中项 / wikilink 跳转目标）。 */
  relPath: string
  /** 是否存在可返回的上一页（App 的回退栈非空）。 */
  canBack: boolean
}>()

const emit = defineEmits<{
  (e: 'back'): void
  (e: 'navigate', pagePath: string): void
}>()

const result = ref<Awaited<ReturnType<typeof readPage>> | null>(null)
const loading = ref(false)
const loadError = ref<string | null>(null)
/** 死链点击提示（几秒后自动消失）。 */
const deadLinkNotice = ref<string | null>(null)
let noticeTimer: number | null = null

/** wikilink 解析器：随共享页面树变化重建（树未就绪时全按死链渲染，
 *  点击时现场重解析兜底）。 */
const resolver = computed(() =>
  buildResolver((pageTree.value?.groups ?? []).flatMap((g) => g.pages)),
)

const fm = computed(() => result.value?.frontmatter ?? null)
const pageTitle = computed(() => fm.value?.title ?? pageDisplayName(props.relPath))
const typeLabel = computed(() =>
  fm.value ? (PAGE_TYPE_LABELS[fm.value.page_type] ?? fm.value.page_type) : null,
)

/** Markdown 正文 → HTML（wikilink 已替换为 a.wikilink / a.wikilink.dead）。 */
const renderedHtml = computed(() =>
  result.value ? renderWikiMarkdown(result.value.body, resolver.value) : '',
)

/** frontmatter 的 related / sources（wikilink 数组）→ 可点击条目。 */
const relatedLinks = computed(() => (fm.value?.related ?? []).map(parseMetaLink))
const sourceLinks = computed(() => (fm.value?.sources ?? []).map(parseMetaLink))

async function load(): Promise<void> {
  const vault = currentVault.value
  if (!vault) return
  loading.value = true
  loadError.value = null
  result.value = null
  try {
    result.value = await readPage(vault.path, props.relPath)
  } catch (err) {
    loadError.value = errorMessage(err)
  } finally {
    loading.value = false
  }
}

watch(() => [currentVault.value?.path, props.relPath] as const, load, {
  immediate: true,
})

/** 正文容器点击委托：a.wikilink → 解析跳转或死链提示。 */
function onBodyClick(ev: MouseEvent): void {
  const el = (ev.target as HTMLElement).closest('a.wikilink')
  if (el === null) return
  ev.preventDefault()
  const target = el.getAttribute('data-target') ?? ''
  const resolved = resolver.value(target)
  if (resolved === null) {
    deadLinkNotice.value = `页面不存在：${target}`
    if (noticeTimer !== null) window.clearTimeout(noticeTimer)
    noticeTimer = window.setTimeout(() => {
      deadLinkNotice.value = null
      noticeTimer = null
    }, 4000)
    return
  }
  emit('navigate', resolved)
}

/** 元信息区的 wikilink 点击（与正文同一解析口径）。 */
function onMetaLinkClick(ev: MouseEvent, target: string): void {
  ev.preventDefault()
  const resolved = resolver.value(target)
  if (resolved === null) {
    deadLinkNotice.value = `页面不存在：${target}`
    if (noticeTimer !== null) window.clearTimeout(noticeTimer)
    noticeTimer = window.setTimeout(() => {
      deadLinkNotice.value = null
      noticeTimer = null
    }, 4000)
    return
  }
  emit('navigate', resolved)
}
</script>

<template>
  <section class="preview">
    <header class="preview-head">
      <button v-if="props.canBack" type="button" class="back-btn" @click="emit('back')">
        ← 返回
      </button>
      <h2 class="preview-title" :title="props.relPath">{{ pageTitle }}</h2>
      <span class="preview-path">{{ props.relPath }}</span>
    </header>

    <div v-if="loading" class="hint">正在读取…</div>
    <p v-else-if="loadError" class="error-text">{{ loadError }}</p>
    <template v-else-if="result">
      <aside v-if="fm" class="meta">
        <div class="meta-row">
          <span v-if="typeLabel" class="badge" :class="`badge-${fm?.page_type}`">{{ typeLabel }}</span>
          <span v-if="fm" class="meta-time">更新于 {{ formatDateTime(fm.updated) }}</span>
        </div>
        <div v-if="fm.aliases.length > 0" class="meta-row">
          <span class="meta-key">别名</span>
          <span v-for="alias in fm.aliases" :key="alias" class="chip">{{ alias }}</span>
        </div>
        <div v-if="fm.tags.length > 0" class="meta-row">
          <span class="meta-key">标签</span>
          <span v-for="tag in fm.tags" :key="tag" class="chip chip-tag">#{{ tag }}</span>
        </div>
        <div v-if="relatedLinks.length > 0" class="meta-row">
          <span class="meta-key">相关</span>
          <template v-for="link in relatedLinks" :key="link.target">
            <a
              href="#"
              class="meta-link"
              :class="{ dead: resolver(link.target) === null }"
              @click="onMetaLinkClick($event, link.target)"
            >{{ link.label }}</a>
          </template>
        </div>
        <div v-if="sourceLinks.length > 0" class="meta-row">
          <span class="meta-key">来源</span>
          <template v-for="link in sourceLinks" :key="link.target">
            <a
              href="#"
              class="meta-link"
              :class="{ dead: resolver(link.target) === null }"
              @click="onMetaLinkClick($event, link.target)"
            >{{ link.label }}</a>
          </template>
        </div>
      </aside>

      <transition name="notice">
        <p v-if="deadLinkNotice" class="dead-notice" role="status">{{ deadLinkNotice }}</p>
      </transition>

      <!-- 安全说明见文件头：v-html 内容为受控 token 转义输出，非裸插值 -->
      <!-- eslint-disable-next-line vue/no-v-html -->
      <article class="wiki-body" @click="onBodyClick" v-html="renderedHtml" />
    </template>
  </section>
</template>

<style scoped>
.preview {
  display: flex;
  flex-direction: column;
  min-height: 0;
  height: 100%;
}

.preview-head {
  display: flex;
  align-items: baseline;
  gap: 14px;
  flex-wrap: wrap;
  padding: 14px 24px 10px;
}

.back-btn {
  padding: 3px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--surface);
  color: var(--text-muted);
  font-size: 12px;
  cursor: pointer;
}

.back-btn:hover {
  color: var(--text-strong);
  background: var(--accent-soft);
}

.back-btn:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.preview-title {
  font-size: 16px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.preview-path {
  font-size: 12px;
  font-family: var(--mono);
  color: var(--text-muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.hint {
  padding: 12px 24px;
  font-size: 13px;
  color: var(--text-muted);
}

.error-text {
  margin: 12px 24px;
  font-size: 13px;
  color: var(--danger);
  word-break: break-all;
}

/* ---- frontmatter 元信息区 ---- */

.meta {
  display: flex;
  flex-direction: column;
  gap: 6px;
  margin: 0 24px 12px;
  padding: 10px 14px;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--bg);
}

.meta-row {
  display: flex;
  align-items: baseline;
  gap: 6px;
  flex-wrap: wrap;
  font-size: 12px;
}

.meta-key {
  color: var(--text-muted);
  flex: none;
  min-width: 30px;
}

.meta-time {
  color: var(--text-muted);
}

.badge {
  padding: 1px 8px;
  border-radius: 999px;
  font-size: 11px;
  font-weight: 600;
  border: 1px solid var(--border);
  color: var(--text-muted);
}

.badge-source {
  background: var(--ok-soft);
  border-color: var(--ok-border);
  color: var(--ok);
}

.badge-entity {
  background: var(--accent-soft);
  border-color: var(--accent-border);
  color: var(--accent);
}

.badge-concept {
  background: var(--danger-soft);
  border-color: var(--danger-border);
  color: var(--danger);
}

.chip {
  padding: 1px 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--surface);
  color: var(--text);
}

.chip-tag {
  color: var(--text-muted);
}

.meta-link {
  color: var(--accent);
  text-decoration: none;
}

.meta-link:hover {
  text-decoration: underline;
}

.meta-link.dead {
  color: var(--danger);
  border-bottom: 1px dashed var(--danger);
}

/* ---- 死链提示条 ---- */

.dead-notice {
  margin: 0 24px 10px;
  padding: 6px 12px;
  border: 1px solid var(--danger-border);
  border-radius: 6px;
  background: var(--danger-soft);
  color: var(--danger);
  font-size: 12px;
}

.notice-enter-active,
.notice-leave-active {
  transition: opacity 0.2s ease;
}

.notice-enter-from,
.notice-leave-to {
  opacity: 0;
}

/* ---- Markdown 正文（v-html 内容不经 scoped 编译，须 :deep） ---- */

.wiki-body {
  flex: 1;
  min-height: 0;
  padding: 4px 24px 32px;
  overflow-y: auto;
  font-size: 14px;
  line-height: 1.75;
  color: var(--text);
}

.wiki-body :deep(h1),
.wiki-body :deep(h2),
.wiki-body :deep(h3),
.wiki-body :deep(h4) {
  margin: 20px 0 8px;
  font-weight: 600;
  line-height: 1.4;
}

.wiki-body :deep(h1) {
  font-size: 20px;
}

.wiki-body :deep(h2) {
  font-size: 17px;
}

.wiki-body :deep(h3) {
  font-size: 15px;
}

.wiki-body :deep(p) {
  margin: 8px 0;
}

.wiki-body :deep(ul),
.wiki-body :deep(ol) {
  margin: 8px 0;
  padding-left: 24px;
}

.wiki-body :deep(li) {
  margin: 3px 0;
}

.wiki-body :deep(blockquote) {
  margin: 10px 0;
  padding: 6px 14px;
  border-left: 3px solid var(--accent-border);
  background: var(--accent-soft);
  border-radius: 0 6px 6px 0;
  color: var(--text-muted);
}

/* 代码块：等宽 + 背景 + 横向滚动（纯 CSS，无语法高亮——体积不值，
 * 后续需要再加） */
.wiki-body :deep(pre) {
  margin: 10px 0;
  padding: 12px 14px;
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow-x: auto;
}

.wiki-body :deep(pre code) {
  padding: 0;
  background: transparent;
  font-size: 12.5px;
  line-height: 1.6;
}

.wiki-body :deep(table) {
  margin: 10px 0;
  border-collapse: collapse;
  font-size: 13px;
  max-width: 100%;
  display: block;
  overflow-x: auto;
}

.wiki-body :deep(th),
.wiki-body :deep(td) {
  border: 1px solid var(--border);
  padding: 5px 10px;
  text-align: left;
}

.wiki-body :deep(th) {
  background: var(--bg);
  font-weight: 600;
}

.wiki-body :deep(hr) {
  margin: 16px 0;
  border: none;
  border-top: 1px solid var(--border);
}

.wiki-body :deep(a.wikilink) {
  color: var(--accent);
  text-decoration: none;
  cursor: pointer;
}

.wiki-body :deep(a.wikilink:hover) {
  text-decoration: underline;
}

/* 死链：红色虚线（样式在渲染期定，点击时现场重解析给提示） */
.wiki-body :deep(a.wikilink.dead) {
  color: var(--danger);
  border-bottom: 1px dashed var(--danger);
}
</style>
