<script setup lang="ts">
/** 新建 Vault 表单（启动页「+ 新建 Vault…」内嵌）。
 *  创建成功 emit('created', root)——由启动页立即打开进入工作区；
 *  目标已是 Vault 时引擎报「该目录已是一个 Vault，请从启动页打开」，
 *  原样内联展示。 */
import { computed, onMounted, ref } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import { createVault, isTauri, listTemplates, type TemplateInfo, type VaultInfo } from '../api'
import { errorMessage, templateDisplayName } from '../display'
import PathPicker from './PathPicker.vue'

const emit = defineEmits<{
  (e: 'created', root: string): void
}>()

const templates = ref<TemplateInfo[]>([])
const templatesError = ref<string | null>(null)
const selectedTemplateId = ref<string | null>(null)
const vaultPath = ref<string | null>(null)
const creating = ref(false)
const createdVault = ref<VaultInfo | null>(null)
const createError = ref<string | null>(null)
/** web 模式的服务器侧目录浏览弹窗（Tauri 下弹系统对话框，不开本弹窗）。 */
const pickerOpen = ref(false)

onMounted(loadTemplates)

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

async function loadTemplates(): Promise<void> {
  try {
    templates.value = await listTemplates()
  } catch (err) {
    templatesError.value = errorMessage(err)
  }
}

function selectTemplate(id: string): void {
  selectedTemplateId.value = id
}

async function chooseDirectory(): Promise<void> {
  // 桌面壳：系统对话框（原样不动）。浏览器：plugin-dialog 依赖 Tauri
  // internals（invoke 直接抛错），改开服务器侧目录浏览弹窗。
  if (isTauri) {
    const dir = await open({ directory: true, multiple: false })
    if (typeof dir === 'string') {
      vaultPath.value = dir
      createError.value = null
    }
    return
  }
  pickerOpen.value = true
}

/** PathPicker 选中（当前目录 / 当前目录+新目录名）：即 Vault 目标路径。 */
function onPicked(path: string): void {
  vaultPath.value = path
  createError.value = null
  pickerOpen.value = false
}

async function handleCreate(): Promise<void> {
  if (!canCreate.value) return
  creating.value = true
  createError.value = null
  createdVault.value = null
  try {
    createdVault.value = await createVault(vaultPath.value!, selectedTemplateId.value!)
    // 成功即打开：启动页接管（openVault → 工作区），本表单随启动页卸载
    emit('created', createdVault.value.root)
  } catch (err) {
    createError.value = errorMessage(err)
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
  <div class="page">
    <section class="card step">
      <h3>1 · 选择模板</h3>
      <div v-if="templatesError" class="load-error">
        <p>模板加载失败：{{ templatesError }}</p>
      </div>
      <div v-else-if="templates.length === 0" class="hint">正在加载模板…</div>
      <div v-else class="template-grid">
        <button
          v-for="t in templates"
          :key="t.id"
          type="button"
          class="template-card"
          :class="{ selected: selectedTemplateId === t.id }"
          @click="selectTemplate(t.id)"
        >
          <span class="template-name">{{ templateDisplayName(t.id, t.name) }}</span>
          <span class="template-id">{{ t.id }}</span>
          <span class="template-desc">{{ t.description }}</span>
        </button>
      </div>
    </section>

    <section class="card step">
      <h3>2 · 选择位置</h3>
      <div class="path-row">
        <output class="path-box" :title="vaultPath ?? undefined">
          {{ vaultPath ?? '尚未选择目录' }}
        </output>
        <button type="button" class="btn" @click="chooseDirectory">
          浏览…
        </button>
      </div>
      <p class="hint">可选择已存在的空目录，或输入尚不存在的新路径；Corpus 会初始化目录骨架并完成 git 初始提交。若所选目录已是一个 Vault，请回到启动页打开它。</p>
    </section>

    <div class="actions">
      <button
        type="button"
        class="btn primary"
        :disabled="!canCreate"
        @click="handleCreate"
      >
        {{ creating ? '创建中…' : '创建 Vault' }}
      </button>
    </div>

    <PathPicker v-if="pickerOpen" mode="dir" @picked="onPicked" @cancel="pickerOpen = false" />

    <section v-if="createError" class="card result error">
      <h3>创建失败</h3>
      <p class="error-text">{{ createError }}</p>
    </section>

    <section v-if="createdVault" class="card result success">
      <h3>Vault 已创建</h3>
      <dl class="meta">
        <div class="meta-row">
          <dt>位置</dt>
          <dd><code>{{ createdVault.root }}</code></dd>
        </div>
        <div class="meta-row">
          <dt>模板</dt>
          <dd>
            {{ templateDisplayName(createdVault.template_id, createdVault.template_name) }}
            （{{ createdVault.template_id }}）
          </dd>
        </div>
        <div class="meta-row">
          <dt>git</dt>
          <dd>已初始化，含初始提交 <code>corpus: init vault</code></dd>
        </div>
      </dl>
      <pre class="structure">{{ structureLines.join('\n') }}</pre>
    </section>
  </div>
</template>

<style scoped>
.page {
  width: 100%;
  display: flex;
  flex-direction: column;
  gap: 20px;
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
