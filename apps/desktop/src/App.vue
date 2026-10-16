<script setup lang="ts">
/** 应用壳（T-UX 两级结构）：
 *  一级·启动页（currentVault 为空）——LauncherPage：最近 Vault / 打开
 *  其他 / 新建；二级·工作区（currentVault 非空）——Obsidian 式四区：
 *  左窄 ribbon + 左文件面板 + 主内容区（功能面板或文件预览，单面板）+ 底部状态栏。
 *  启动恢复：取最近列表第一名自动 open_vault，失败回启动页并提示。 */
import { onMounted, ref } from 'vue'
import LauncherPage from './components/LauncherPage.vue'
import WorkspaceRibbon from './components/WorkspaceRibbon.vue'
import FilePanel from './components/FilePanel.vue'
import PagePreview from './components/PagePreview.vue'
import IngestPage from './components/IngestPage.vue'
import SettingsPage from './components/SettingsPage.vue'
import StatusBar from './components/StatusBar.vue'
import { listRecentVaults, type PageEntry } from './api'
import { errorMessage } from './display'
import { closeVault, currentVault, openVault } from './vault-state'

/** 主区形态：功能面板（摄取 / 设置）或文件预览。
 *  多标签页与右侧边栏明确不做（T4 随页面浏览一起做）。 */
type MainPanel = 'ingest' | 'settings' | 'preview'

const booting = ref(true)
const startupError = ref<string | null>(null)
const activePanel = ref<MainPanel>('ingest')
/** 当前选中页面（Obsidian 式：切面板不清选中，切 Vault 才清）。 */
const selectedPage = ref<PageEntry | null>(null)
/** 设置保存后递增：经 :key 重挂 StatusBar，取到新的 Apply 模式。 */
const configVersion = ref(0)

onMounted(async () => {
  try {
    const recent = await listRecentVaults()
    if (recent.length > 0) {
      // 自动恢复最近打开的 Vault；打不开（目录被移走 / 非 Vault）静默
      // 回启动页，错误经 startupError 展示
      await openVault(recent[0].path)
    }
  } catch (err) {
    startupError.value = errorMessage(err)
  } finally {
    booting.value = false
  }
})

function selectPanel(panel: 'ingest' | 'settings'): void {
  activePanel.value = panel
}

function openPreview(page: PageEntry): void {
  selectedPage.value = page
  activePanel.value = 'preview'
}

function onConfigSaved(): void {
  configVersion.value++
}

/** 切换 Vault：回启动页，清选中与面板态。 */
function switchVault(): void {
  closeVault()
  activePanel.value = 'ingest'
  selectedPage.value = null
}
</script>

<template>
  <div v-if="booting" class="boot">正在启动…</div>

  <LauncherPage v-else-if="!currentVault" :startup-error="startupError" />

  <div v-else class="workspace">
    <WorkspaceRibbon
      :active="activePanel === 'preview' ? null : activePanel"
      @select-panel="selectPanel"
      @switch-vault="switchVault"
    />
    <FilePanel :selected="selectedPage?.path ?? null" @select="openPreview" />
    <main class="main-area">
      <IngestPage v-if="activePanel === 'ingest'" />
      <SettingsPage v-else-if="activePanel === 'settings'" @saved="onConfigSaved" />
      <PagePreview v-else-if="selectedPage" :rel-path="selectedPage.path" />
    </main>
    <StatusBar :key="configVersion" :selected-page="selectedPage" />
  </div>
</template>

<style scoped>
.boot {
  min-height: 100vh;
  display: grid;
  place-items: center;
  color: var(--text-muted);
  font-size: 14px;
}

.workspace {
  height: 100vh;
  display: grid;
  grid-template-columns: 48px 232px 1fr;
  grid-template-rows: 1fr auto;
  grid-template-areas:
    'ribbon files main'
    'status status status';
  overflow: hidden;
}

.workspace > nav {
  grid-area: ribbon;
}

.workspace > aside {
  grid-area: files;
}

.main-area {
  grid-area: main;
  min-width: 0;
  min-height: 0;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
}

.main-area > * {
  flex: 1;
  min-height: 0;
}

.workspace > footer {
  grid-area: status;
}
</style>
