<script setup lang="ts">
/** 工作区底部状态栏：左（模板名 · Vault 路径）右（Apply 模式 · 选中页
 *  的 updated 时间）。Apply 模式读全局配置；设置保存后 App 通过 :key
 *  重挂本组件取到新值。选中页信息来自 list_pages 条目（updated 为空
 *  不显示）。 */
import { onMounted, ref } from 'vue'
import { getConfig, type AppConfig } from '../api'
import { errorMessage, formatDateTime, pageDisplayName } from '../display'
import { currentVault } from '../vault-state'

/** 当前选中页面（null = 未选中，右侧不显示 updated）。 */
const props = defineProps<{
  selectedPage: { path: string; updated: string | null } | null
}>()

const APPLY_MODE_LABELS: Record<AppConfig['apply_mode'], string> = {
  manual: 'Apply：人审触发',
  auto: 'Apply：自动执行',
}

const applyMode = ref<AppConfig['apply_mode'] | null>(null)
const loadError = ref<string | null>(null)

onMounted(async () => {
  try {
    applyMode.value = (await getConfig()).apply_mode
  } catch (err) {
    loadError.value = errorMessage(err)
  }
})
</script>

<template>
  <footer class="status-bar">
    <span v-if="currentVault" class="status-item template" :title="`模板：${currentVault.templateName}（${currentVault.templateId}）`">
      {{ currentVault.templateName }}
    </span>
    <span v-if="currentVault" class="status-item path" :title="currentVault.path">
      {{ currentVault.path }}
    </span>
    <span class="spacer" />
    <span
      v-if="props.selectedPage"
      class="status-item"
      :title="`选中页面：${props.selectedPage.path}`"
    >
      {{ pageDisplayName(props.selectedPage.path) }}
      <template v-if="props.selectedPage.updated">
        · 更新于 {{ formatDateTime(props.selectedPage.updated) }}
      </template>
    </span>
    <span v-if="applyMode" class="status-item apply" :class="applyMode">
      {{ APPLY_MODE_LABELS[applyMode] }}
    </span>
    <span v-else-if="loadError" class="status-item error" :title="loadError">配置读取失败</span>
  </footer>
</template>

<style scoped>
.status-bar {
  display: flex;
  align-items: center;
  gap: 14px;
  padding: 3px 12px;
  background: var(--surface);
  border-top: 1px solid var(--border);
  font-size: 12px;
  color: var(--text-muted);
  overflow: hidden;
}

.spacer {
  flex: 1;
}

.status-item {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  min-width: 0;
}

.status-item.path {
  font-family: var(--mono);
  font-size: 11px;
  max-width: 45%;
}

.status-item.template {
  color: var(--text);
  font-weight: 600;
  flex: none;
}

.status-item.apply {
  flex: none;
  padding: 0 8px;
  border-radius: 999px;
  border: 1px solid var(--border);
  background: var(--bg);
}

.status-item.apply.auto {
  color: var(--accent);
  border-color: var(--accent-border);
  background: var(--accent-soft);
}

.status-item.error {
  color: var(--danger);
  flex: none;
}
</style>
