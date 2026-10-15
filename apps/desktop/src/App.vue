<script setup lang="ts">
import { computed, ref } from 'vue'
import { invoke } from '@tauri-apps/api/core'
import { open } from '@tauri-apps/plugin-dialog'

/** create_vault 命令的返回结构（字段为引擎 serde 的 snake_case） */
interface VaultInfo {
  root: string
  template_id: string
  template_name: string
  created_paths: string[]
}

/** 模板选择项：id 与引擎编译期常量（generic / research / reading）对应 */
interface TemplateOption {
  id: string
  name: string
  description: string
}

const templates: TemplateOption[] = [
  {
    id: 'generic',
    name: '通用知识库',
    description: '以 Entity / Concept / Topic 组织日常积累，适合大多数场景。',
  },
  {
    id: 'research',
    name: '研究',
    description: '围绕课题的问题、方法与文献脉络组织页面，便于追源与对比。',
  },
  {
    id: 'reading',
    name: '阅读',
    description: '以书摘、作者、人物与主题沉淀长期阅读，构建个人阅读图谱。',
  },
]

const selectedTemplateId = ref<string | null>(null)
const vaultPath = ref<string | null>(null)
const creating = ref(false)
const createdVault = ref<VaultInfo | null>(null)
const errorMessage = ref<string | null>(null)

const canCreate = computed(
  () =>
    selectedTemplateId.value !== null &&
    vaultPath.value !== null &&
    !creating.value,
)

/** 目录结构展示（树形文本） */
const structureLines = computed(() => {
  const info = createdVault.value
  if (!info) return []
  const rootName = info.root.split('/').filter(Boolean).pop() ?? info.root
  return buildTreeLines(info.created_paths, rootName)
})

function selectTemplate(id: string): void {
  selectedTemplateId.value = id
}

async function chooseDirectory(): Promise<void> {
  const dir = await open({ directory: true, multiple: false })
  if (typeof dir === 'string') {
    vaultPath.value = dir
    errorMessage.value = null
  }
}

async function createVault(): Promise<void> {
  if (!canCreate.value) return
  creating.value = true
  errorMessage.value = null
  createdVault.value = null
  try {
    createdVault.value = await invoke<VaultInfo>('create_vault', {
      path: vaultPath.value,
      templateId: selectedTemplateId.value,
    })
  } catch (err) {
    errorMessage.value = typeof err === 'string' ? err : JSON.stringify(err)
  } finally {
    creating.value = false
  }
}

/** 把相对路径清单渲染成树形文本 */
function buildTreeLines(paths: string[], rootLabel: string): string[] {
  interface Node {
    name: string
    children: Map<string, Node>
  }
  const root: Node = { name: rootLabel, children: new Map() }
  for (const p of paths) {
    let node = root
    for (const part of p.split('/').filter(Boolean)) {
      let child = node.children.get(part)
      if (!child) {
        child = { name: part, children: new Map() }
        node.children.set(part, child)
      }
      node = child
    }
  }
  const lines: string[] = [`${rootLabel}/`]
  const walk = (node: Node, prefix: string): void => {
    const entries = [...node.children.values()].sort((a, b) => {
      const aDir = a.children.size > 0
      const bDir = b.children.size > 0
      if (aDir !== bDir) return aDir ? -1 : 1
      return a.name.localeCompare(b.name)
    })
    entries.forEach((child, i) => {
      const last = i === entries.length - 1
      const isDir = child.children.size > 0
      lines.push(
        `${prefix}${last ? '└── ' : '├── '}${child.name}${isDir ? '/' : ''}`,
      )
      walk(child, prefix + (last ? '    ' : '│   '))
    })
  }
  walk(root, '')
  return lines
}
</script>

<template>
  <main class="page">
    <header class="page-header">
      <h1>Corpus</h1>
      <p class="subtitle">创建 Vault：一个目录即一个 git 仓库（Raw sources + Wiki + Purpose）</p>
    </header>

    <section class="card step">
      <h2>1 · 选择模板</h2>
      <div class="template-grid">
        <button
          v-for="t in templates"
          :key="t.id"
          type="button"
          class="template-card"
          :class="{ selected: selectedTemplateId === t.id }"
          @click="selectTemplate(t.id)"
        >
          <span class="template-name">{{ t.name }}</span>
          <span class="template-id">{{ t.id }}</span>
          <span class="template-desc">{{ t.description }}</span>
        </button>
      </div>
    </section>

    <section class="card step">
      <h2>2 · 选择位置</h2>
      <div class="path-row">
        <output class="path-box" :title="vaultPath ?? undefined">
          {{ vaultPath ?? '尚未选择目录' }}
        </output>
        <button type="button" class="btn" @click="chooseDirectory">
          浏览…
        </button>
      </div>
      <p class="hint">目标目录必须不存在，Corpus 会新建它并完成 git 初始提交。</p>
    </section>

    <div class="actions">
      <button
        type="button"
        class="btn primary"
        :disabled="!canCreate"
        @click="createVault"
      >
        {{ creating ? '创建中…' : '创建 Vault' }}
      </button>
    </div>

    <section v-if="errorMessage" class="card result error">
      <h2>创建失败</h2>
      <p class="error-text">{{ errorMessage }}</p>
    </section>

    <section v-if="createdVault" class="card result success">
      <h2>Vault 已创建</h2>
      <dl class="meta">
        <div class="meta-row">
          <dt>位置</dt>
          <dd><code>{{ createdVault.root }}</code></dd>
        </div>
        <div class="meta-row">
          <dt>模板</dt>
          <dd>{{ createdVault.template_name }}（{{ createdVault.template_id }}）</dd>
        </div>
        <div class="meta-row">
          <dt>git</dt>
          <dd>已初始化，含初始提交 <code>corpus: init vault</code></dd>
        </div>
      </dl>
      <pre class="structure">{{ structureLines.join('\n') }}</pre>
    </section>
  </main>
</template>

<style scoped>
.page {
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

.step h2 {
  font-size: 15px;
  margin-bottom: 14px;
}

.card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 20px;
}

.template-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 12px;
}

@media (max-width: 720px) {
  .template-grid {
    grid-template-columns: 1fr;
  }
}

.template-card {
  display: flex;
  flex-direction: column;
  gap: 6px;
  text-align: left;
  padding: 16px;
  border-radius: 8px;
  border: 1.5px solid var(--border);
  background: var(--surface);
  color: var(--text);
  cursor: pointer;
  transition:
    border-color 0.15s ease,
    background 0.15s ease;
}

.template-card:hover {
  border-color: var(--accent-border);
}

.template-card.selected {
  border-color: var(--accent);
  background: var(--accent-soft);
}

.template-card:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 2px;
}

.template-name {
  color: var(--text-strong);
  font-weight: 600;
  font-size: 16px;
}

.template-id {
  font-family: var(--mono);
  font-size: 12px;
  color: var(--text-muted);
}

.template-desc {
  font-size: 13px;
  line-height: 1.5;
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

.result.error {
  border-color: var(--danger-border);
  background: var(--danger-soft);
}

.result.error h2 {
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

.result.success h2 {
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

.structure {
  margin: 14px 0 0;
  padding: 14px;
  border: 1px solid var(--border);
  border-radius: 8px;
  background: var(--surface);
  font-family: var(--mono);
  font-size: 13px;
  line-height: 1.6;
  overflow-x: auto;
}
</style>
