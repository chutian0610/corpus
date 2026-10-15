<script setup lang="ts">
import { ref } from 'vue'
import SettingsPage from './components/SettingsPage.vue'
import VaultCreatePage from './components/VaultCreatePage.vue'

type PageId = 'create' | 'settings'

const page = ref<PageId>('create')
</script>

<template>
  <div class="app-shell">
    <nav class="topnav">
      <span class="brand">Corpus</span>
      <div class="tabs">
        <button
          type="button"
          class="tab"
          :class="{ active: page === 'create' }"
          @click="page = 'create'"
        >
          创建 Vault
        </button>
        <button
          type="button"
          class="tab"
          :class="{ active: page === 'settings' }"
          @click="page = 'settings'"
        >
          设置
        </button>
      </div>
    </nav>
    <VaultCreatePage v-if="page === 'create'" />
    <SettingsPage v-else />
  </div>
</template>

<style scoped>
.app-shell {
  min-height: 100vh;
  display: flex;
  flex-direction: column;
}

.topnav {
  position: sticky;
  top: 0;
  z-index: 10;
  display: flex;
  align-items: center;
  gap: 20px;
  padding: 10px 24px;
  background: var(--surface);
  border-bottom: 1px solid var(--border);
}

.brand {
  color: var(--text-strong);
  font-weight: 700;
  font-size: 17px;
  letter-spacing: 0.5px;
}

.tabs {
  display: flex;
  gap: 6px;
}

.tab {
  padding: 6px 14px;
  border: none;
  border-radius: 6px;
  background: transparent;
  color: var(--text-muted);
  font-size: 14px;
  cursor: pointer;
}

.tab:hover {
  color: var(--text-strong);
  background: var(--accent-soft);
}

.tab.active {
  color: var(--accent);
  background: var(--accent-soft);
  font-weight: 600;
}
</style>
