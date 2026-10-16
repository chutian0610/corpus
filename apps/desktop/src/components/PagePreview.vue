<script setup lang="ts">
/** 主区页面预览：只读原始文本（<pre> 级），顶部注明正式渲染属 T4。
 *  wikilink / frontmatter 的富渲染是页面浏览票据（T4）的范围。 */
import { ref, watch } from 'vue'
import { readPage } from '../api'
import { errorMessage } from '../display'
import { currentVault } from '../vault-state'

const props = defineProps<{
  /** 相对 wiki/ 的页面路径（FilePanel 选中项）。 */
  relPath: string
}>()

const content = ref<string | null>(null)
const loading = ref(false)
const loadError = ref<string | null>(null)

async function load(): Promise<void> {
  const vault = currentVault.value
  if (!vault) return
  loading.value = true
  loadError.value = null
  content.value = null
  try {
    content.value = await readPage(vault.path, props.relPath)
  } catch (err) {
    loadError.value = errorMessage(err)
  } finally {
    loading.value = false
  }
}

watch(() => [currentVault.value?.path, props.relPath] as const, load, {
  immediate: true,
})
</script>

<template>
  <section class="preview">
    <header class="preview-head">
      <h2 class="preview-title" :title="props.relPath">{{ props.relPath }}</h2>
      <span class="preview-note">原始文本预览 · 正式渲染（wikilink / frontmatter）属 T4</span>
    </header>

    <div v-if="loading" class="hint">正在读取…</div>
    <p v-else-if="loadError" class="error-text">{{ loadError }}</p>
    <pre v-else-if="content" class="raw">{{ content }}</pre>
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

.preview-title {
  font-size: 15px;
  font-family: var(--mono);
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.preview-note {
  font-size: 12px;
  color: var(--text-muted);
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

.raw {
  flex: 1;
  min-height: 0;
  margin: 0;
  padding: 4px 24px 32px;
  border: none;
  background: transparent;
  font-family: var(--mono);
  font-size: 13px;
  line-height: 1.7;
  color: var(--text);
  white-space: pre-wrap;
  word-break: break-word;
  overflow-y: auto;
}
</style>
