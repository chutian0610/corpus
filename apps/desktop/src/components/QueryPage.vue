<script setup lang="ts">
/** 查询面板（T9 查询最小闭环）：输入问题 → 流式回答（词法检索 → LLM
 *  综合）→ 引用页面列表 → 会话内问答历史。
 *
 *  回答里的 [[wikilink]] 复用 wiki-links 渲染管线（与 PagePreview 同一
 *  安全模型：markdown-it html:false + inline rule 受控 <a>，v-html 内容
 *  为受控 token 转义输出），可点击跳转（navigate 委托给 App 的预览路由）。
 *  引用列表条目同理可点击。流式回答区按引擎口径只展示字数（增量内容
 *  完成后随结果整体到达）。 */
import { computed, ref } from 'vue'
import type { QueryCitation } from '../api'
import {
  answerChars,
  askQuestion,
  asking,
  history,
  phase,
  queryError,
  searchTerms,
} from '../query-state'
import { currentVault, pageTree } from '../vault-state'
import { buildResolver, renderWikiMarkdown } from '../wiki-links'

const emit = defineEmits<{
  (e: 'navigate', pagePath: string): void
}>()

const question = ref('')

/** 查询只作用于当前打开的 Vault（工作区面板；无 Vault 时不会挂载）。 */
const vault = computed(() => currentVault.value)
const canAsk = computed(
  () => vault.value !== null && question.value.trim() !== '' && !asking.value,
)

/** wikilink 解析器：与 PagePreview 同源（共享页面树）。 */
const resolver = computed(() =>
  buildResolver((pageTree.value?.groups ?? []).flatMap((g) => g.pages)),
)

/** 历史倒序展示（最新在最上）。 */
const turnsNewestFirst = computed(() => [...history.value].reverse())

/** 回答 Markdown → HTML（wikilink 已替换为 a.wikilink / a.wikilink.dead）。 */
function renderAnswer(answer: string): string {
  return renderWikiMarkdown(answer, resolver.value)
}

function ask(): void {
  if (vault.value === null) return
  const q = question.value
  question.value = ''
  void askQuestion(vault.value.path, q)
}

/** 回答区 / 引用列表的点击委托：a.wikilink → 跳转（死链交由预览面板
 *  的既有死链提示口径处理，这里只解析命中的）。 */
function onBodyClick(ev: MouseEvent): void {
  const el = (ev.target as HTMLElement).closest('a.wikilink')
  if (el === null) return
  ev.preventDefault()
  const target = el.getAttribute('data-target') ?? ''
  const resolved = resolver.value(target)
  if (resolved !== null) emit('navigate', resolved)
}

/** 引用列表条目点击：直接跳引用页。 */
function openCitation(citation: QueryCitation): void {
  emit('navigate', citation.path)
}
</script>

<template>
  <main class="page">
    <header class="page-header">
      <h2>查询（Query）</h2>
      <p class="subtitle">自然语言提问：词法候选页（零 LLM）→ LLM 综合成答（引用为可点击 wikilink）→ 默认沉淀 Query page（配置可关）</p>
      <p v-if="vault" class="subtitle vault-line">
        当前 Vault：<code :title="vault.path">{{ vault.path }}</code>
      </p>
    </header>

    <section class="card">
      <label class="field-label" for="query-input">向当前 Vault 提问</label>
      <div class="ask-row">
        <input
          id="query-input"
          v-model="question"
          type="text"
          class="ask-input"
          placeholder="例如：Transformer 的核心机制是什么？"
          :disabled="asking"
          @keydown.enter="ask"
        />
        <button type="button" class="btn primary" :disabled="!canAsk" @click="ask">
          {{ asking ? '回答中…' : '提问' }}
        </button>
      </div>

      <div v-if="asking" class="progress" role="status">
        <template v-if="phase === 'searching'">
          <span class="progress-line">正在检索候选页（词法匹配，零 LLM）…</span>
          <span v-if="searchTerms.length > 0" class="terms">
            <span v-for="term in searchTerms.slice(0, 12)" :key="term" class="chip">{{ term }}</span>
            <span v-if="searchTerms.length > 12" class="term-more">+{{ searchTerms.length - 12 }}</span>
          </span>
        </template>
        <template v-else-if="phase === 'answering'">
          <span class="progress-line">正在综合回答…已接收 {{ answerChars }} 字（模型输出中）</span>
        </template>
      </div>

      <p v-if="queryError" class="error-text" role="alert">{{ queryError }}</p>
      <p v-if="phase === 'error' && queryError" class="hint">
        查询失败：Wiki 与 git 保持原样，无半写状态。
      </p>
    </section>

    <section v-for="(turn, idx) in turnsNewestFirst" :key="history.length - idx" class="card turn">
      <h3 class="turn-question">{{ turn.question }}</h3>
      <!-- 安全说明同 PagePreview：v-html 内容为受控 token 转义输出，非裸插值 -->
      <!-- eslint-disable-next-line vue/no-v-html -->
      <article class="answer-body" @click="onBodyClick" v-html="renderAnswer(turn.answer)" />
      <div v-if="turn.citations.length > 0" class="citations">
        <span class="citations-label">引用页面（{{ turn.citations.length }}）</span>
        <button
          v-for="citation in turn.citations"
          :key="citation.path"
          type="button"
          class="citation-link"
          :title="citation.path"
          @click="openCitation(citation)"
        >{{ citation.title }}</button>
      </div>
      <p v-if="turn.queryPagePath" class="settled-note">
        已沉淀 <code>{{ turn.queryPagePath }}</code>
      </p>
    </section>

    <p v-if="history.length === 0 && !asking && phase !== 'error'" class="hint empty-hint">
      还没有提问。回答中的引用与历史列表里的页面都可以点击跳转。
    </p>
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

.card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 20px;
}

.field-label {
  display: block;
  margin-bottom: 8px;
  font-size: 13px;
  color: var(--text-muted);
}

.ask-row {
  display: flex;
  gap: 10px;
}

.ask-input {
  flex: 1;
  min-width: 0;
  padding: 8px 12px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--bg);
  color: var(--text-strong);
  font-size: 14px;
}

.ask-input:focus-visible {
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

.btn.primary:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

.progress {
  margin-top: 14px;
  display: flex;
  flex-direction: column;
  gap: 8px;
  font-size: 13px;
  color: var(--text-strong);
}

.terms {
  display: flex;
  flex-wrap: wrap;
  gap: 4px;
}

.term-more {
  font-size: 11px;
  color: var(--text-muted);
  align-self: center;
}

.chip {
  padding: 1px 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--bg);
  color: var(--text-muted);
  font-family: var(--mono);
  font-size: 11px;
}

.error-text {
  margin-top: 12px;
  font-size: 13px;
  color: var(--danger);
  word-break: break-all;
}

.hint {
  font-size: 13px;
  color: var(--text-muted);
}

.empty-hint {
  text-align: center;
}

.turn {
  display: flex;
  flex-direction: column;
  gap: 10px;
}

.turn-question {
  margin: 0;
  font-size: 15px;
  color: var(--text-strong);
  word-break: break-all;
}

/* ---- 回答正文（v-html 内容不经 scoped 编译，须 :deep） ---- */

.answer-body {
  font-size: 14px;
  line-height: 1.75;
  color: var(--text);
  word-break: break-word;
}

.answer-body :deep(p) {
  margin: 8px 0;
}

.answer-body :deep(ul),
.answer-body :deep(ol) {
  margin: 8px 0;
  padding-left: 24px;
}

.answer-body :deep(pre) {
  margin: 10px 0;
  padding: 12px 14px;
  background: var(--bg);
  border: 1px solid var(--border);
  border-radius: 8px;
  overflow-x: auto;
}

.answer-body :deep(pre code) {
  padding: 0;
  background: transparent;
  font-size: 12.5px;
}

.answer-body :deep(a.wikilink) {
  color: var(--accent);
  text-decoration: none;
  cursor: pointer;
}

.answer-body :deep(a.wikilink:hover) {
  text-decoration: underline;
}

.answer-body :deep(a.wikilink.dead) {
  color: var(--danger);
  border-bottom: 1px dashed var(--danger);
}

/* ---- 引用列表 ---- */

.citations {
  display: flex;
  align-items: baseline;
  gap: 8px;
  flex-wrap: wrap;
  padding-top: 10px;
  border-top: 1px solid var(--border);
}

.citations-label {
  font-size: 12px;
  color: var(--text-muted);
  flex: none;
}

.citation-link {
  padding: 2px 10px;
  border: 1px solid var(--accent-border);
  border-radius: 999px;
  background: var(--accent-soft);
  color: var(--accent);
  font-size: 12px;
  cursor: pointer;
}

.citation-link:hover {
  text-decoration: underline;
}

.citation-link:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.settled-note {
  margin: 0;
  font-size: 12px;
  color: var(--text-muted);
}

.settled-note code {
  font-family: var(--mono);
  font-size: 11px;
}
</style>
