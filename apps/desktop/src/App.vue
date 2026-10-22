<script setup lang="ts">
import { ref } from 'vue'
import IngestPage from './components/IngestPage.vue'
import SettingsPage from './components/SettingsPage.vue'
import VaultCreatePage from './components/VaultCreatePage.vue'
import { currentVault } from './vault-state'

type PageId = 'create' | 'ingest' | 'settings'

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
          :class="{ active: page === 'ingest' }"
          @click="page = 'ingest'"
        >
          摄取
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
      <span
        v-if="currentVault"
        class="current-vault"
        :title="currentVault.path"
      >
        当前 Vault：{{ currentVault.path }}
      </span>
    </nav>
    <VaultCreatePage v-if="page === 'create'" />
    <IngestPage v-else-if="page === 'ingest'" />
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

.current-vault {
  margin-left: auto;
  min-width: 0;
  max-width: 45%;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-family: var(--mono);
  font-size: 12px;
  color: var(--text-muted);
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
