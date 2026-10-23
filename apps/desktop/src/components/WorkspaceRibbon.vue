<script setup lang="ts">
/** 工作区左窄 ribbon（图标竖条，Obsidian 式）：
 *  上部功能入口（摄取 / 查询可用；图谱/Lint 置灰并标注后续票据），
 *  底部设置齿轮与「切换 Vault」（回启动页）。
 *  图标为内联 SVG（stroke 风格），不引入图标库依赖。 */
defineProps<{
  /** 高亮的活动入口（'ingest' | 'query' | 'settings'）；文件预览时无功能入口高亮。 */
  active: 'ingest' | 'query' | 'settings' | null
}>()

const emit = defineEmits<{
  (e: 'selectPanel', panel: 'ingest' | 'query' | 'settings'): void
  (e: 'switchVault'): void
}>()
</script>

<template>
  <nav class="ribbon" aria-label="功能导航">
    <div class="ribbon-top">
      <button
        type="button"
        class="ribbon-btn"
        :class="{ active: active === 'ingest' }"
        title="摄取（Ingest）：把 raw/ 文档编译进 Wiki"
        aria-label="摄取"
        @click="emit('selectPanel', 'ingest')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M12 3v11" />
          <path d="m7 10 5 5 5-5" />
          <path d="M4 19h16" />
        </svg>
        <span class="ribbon-label">摄取</span>
      </button>

      <button
        type="button"
        class="ribbon-btn"
        :class="{ active: active === 'query' }"
        title="查询（Query）：自然语言提问，词法候选 → LLM 综合 → 沉淀 Query page"
        aria-label="查询"
        @click="emit('selectPanel', 'query')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="11" cy="11" r="7" />
          <path d="m20 20-3.8-3.8" />
        </svg>
        <span class="ribbon-label">查询</span>
      </button>

      <button
        type="button"
        class="ribbon-btn disabled"
        disabled
        title="图谱 · 属后续票据 T10"
        aria-label="图谱（未实现，属票据 T10）"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="6" cy="6" r="2.5" />
          <circle cx="18" cy="7" r="2.5" />
          <circle cx="12" cy="18" r="2.5" />
          <path d="m8.2 7.3 7.5-.8" />
          <path d="m7 8.4 3.8 7.3" />
          <path d="m16.8 9.2-3.3 6.6" />
        </svg>
        <span class="ribbon-label">图谱</span>
        <span class="ribbon-badge">T10</span>
      </button>

      <button
        type="button"
        class="ribbon-btn disabled"
        disabled
        title="Lint 一致性检查 · 属后续票据 T11"
        aria-label="Lint（未实现，属票据 T11）"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M9 3h6" />
          <path d="M10 3v5L4.8 17a2.4 2.4 0 0 0 2.1 3.6h10.2a2.4 2.4 0 0 0 2.1-3.6L14 8V3" />
          <path d="m7.5 14 9 .5" />
        </svg>
        <span class="ribbon-label">Lint</span>
        <span class="ribbon-badge">T11</span>
      </button>
    </div>

    <div class="ribbon-bottom">
      <button
        type="button"
        class="ribbon-btn"
        :class="{ active: active === 'settings' }"
        title="设置（模型与行为配置）"
        aria-label="设置"
        @click="emit('selectPanel', 'settings')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <circle cx="12" cy="12" r="3.2" />
          <path d="M19 12a7 7 0 0 0-.1-1.2l2-1.5-2-3.5-2.4 1a7 7 0 0 0-2-1.2L14 3h-4l-.4 2.6a7 7 0 0 0-2 1.2l-2.5-1-2 3.5 2 1.5A7 7 0 0 0 5 12a7 7 0 0 0 .1 1.2l-2 1.5 2 3.5 2.4-1a7 7 0 0 0 2 1.2L10 21h4l.4-2.6a7 7 0 0 0 2-1.2l2.5 1 2-3.5-2-1.5c.06-.4.1-.8.1-1.2Z" />
        </svg>
      </button>

      <button
        type="button"
        class="ribbon-btn"
        title="切换 Vault（回到启动页）"
        aria-label="切换 Vault"
        @click="emit('switchVault')"
      >
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
          <path d="M12 3a9 9 0 1 1-8.5 6" />
          <path d="M12 7v5" />
          <path d="M3.5 3.5 4 9l5.5-.5" />
        </svg>
      </button>
    </div>
  </nav>
</template>

<style scoped>
.ribbon {
  display: flex;
  flex-direction: column;
  justify-content: space-between;
  align-items: center;
  padding: 10px 0;
  background: var(--surface);
  border-right: 1px solid var(--border);
}

.ribbon-top,
.ribbon-bottom {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
}

.ribbon-btn {
  position: relative;
  width: 36px;
  height: 36px;
  display: grid;
  place-items: center;
  border: none;
  border-radius: 8px;
  background: transparent;
  color: var(--text-muted);
  cursor: pointer;
}

.ribbon-btn svg {
  width: 20px;
  height: 20px;
}

.ribbon-btn:hover:not(.disabled) {
  background: var(--accent-soft);
  color: var(--text-strong);
}

.ribbon-btn:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.ribbon-btn.active {
  background: var(--accent-soft);
  color: var(--accent);
}

.ribbon-btn.disabled {
  opacity: 0.35;
  cursor: not-allowed;
}

/* 功能入口的文字标签：窄条下仍可辨（无障碍兜底，视觉以图标为主） */
.ribbon-label {
  position: absolute;
  width: 1px;
  height: 1px;
  overflow: hidden;
  clip: rect(0 0 0 0);
  white-space: nowrap;
}

/* 后续票据角标 */
.ribbon-badge {
  position: absolute;
  top: -2px;
  right: -4px;
  font-size: 9px;
  line-height: 1;
  padding: 2px 3px;
  border-radius: 4px;
  background: var(--border);
  color: var(--text-muted);
  font-family: var(--mono);
}
</style>
