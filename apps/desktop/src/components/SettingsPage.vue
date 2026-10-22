<script setup lang="ts">
/** 设置面板（工作区主区）：模型接入与全局行为配置。
 *  T-UX 起「最近 Vault」卡片删除（职责移交启动页），本面板只管
 *  全局模型 / 行为配置；保存成功 emit('saved')，App 据此刷新状态栏。 */
import { onMounted, reactive, ref } from 'vue'
import {
  getConfig,
  setConfig,
  testConnection,
  type AppConfig,
  type ConnectionReport,
  type ModelTier,
} from '../api'
import { PROVIDER_OPTIONS, errorMessage } from '../display'

/** 表单内的档位：可空字段用空串编辑，提交时转回 null */
interface TierForm {
  provider: string
  model: string
  base_url: string
  api_key: string
}

const TIERS: ReadonlyArray<{ key: 'fast' | 'strong'; label: string; hint: string }> = [
  { key: 'fast', label: '快速档', hint: '高频、低延迟任务（页面路由、摘要等）' },
  { key: 'strong', label: '强力档', hint: '复杂推理任务（成文、矛盾检测等）' },
]

const emit = defineEmits<{
  (e: 'saved'): void
}>()

const form = reactive({
  fast: { provider: 'openai', model: '', base_url: '', api_key: '' } as TierForm,
  strong: { provider: 'openai', model: '', base_url: '', api_key: '' } as TierForm,
  apply_mode: 'manual' as AppConfig['apply_mode'],
  query_pages: true,
})

/** API key 输入框的显隐状态（password ↔ text，每档位独立）。 */
const showApiKey = reactive({ fast: false, strong: false })

const loading = ref(true)
const loadError = ref<string | null>(null)
const saveState = ref<'idle' | 'saving' | 'saved'>('idle')
const saveError = ref<string | null>(null)

const testing = reactive({ fast: false, strong: false })
const testResult = reactive({
  fast: null as ConnectionReport | null,
  strong: null as ConnectionReport | null,
})
const testError = reactive({ fast: null as string | null, strong: null as string | null })

onMounted(async () => {
  await loadConfig()
  loading.value = false
})

async function loadConfig(): Promise<void> {
  try {
    const config = await getConfig()
    form.fast = toTierForm(config.fast)
    form.strong = toTierForm(config.strong)
    form.apply_mode = config.apply_mode
    form.query_pages = config.query_pages
  } catch (err) {
    loadError.value = errorMessage(err)
  }
}

function toTierForm(tier: ModelTier): TierForm {
  return {
    provider: tier.provider,
    model: tier.model,
    base_url: tier.base_url ?? '',
    api_key: tier.api_key ?? '',
  }
}

function toModelTier(tier: TierForm): ModelTier {
  return {
    provider: tier.provider,
    model: tier.model.trim(),
    base_url: tier.base_url.trim() || null,
    api_key: tier.api_key.trim() || null,
  }
}

async function saveConfig(): Promise<void> {
  saveState.value = 'saving'
  saveError.value = null
  try {
    const config: AppConfig = {
      fast: toModelTier(form.fast),
      strong: toModelTier(form.strong),
      apply_mode: form.apply_mode,
      query_pages: form.query_pages,
    }
    await setConfig(config)
    saveState.value = 'saved'
    emit('saved')
    setTimeout(() => {
      if (saveState.value === 'saved') saveState.value = 'idle'
    }, 2000)
  } catch (err) {
    saveState.value = 'idle'
    saveError.value = errorMessage(err)
  }
}

/** 测试连通：对当前表单中的档位配置发起真实最小调用 */
async function runTestConnection(key: 'fast' | 'strong'): Promise<void> {
  testing[key] = true
  testResult[key] = null
  testError[key] = null
  try {
    testResult[key] = await testConnection(toModelTier(form[key]))
  } catch (err) {
    testError[key] = errorMessage(err)
  } finally {
    testing[key] = false
  }
}
</script>

<template>
  <main class="page">
    <header class="page-header">
      <h2>设置</h2>
      <p class="subtitle">模型接入与全局配置（保存在应用数据目录的 config.db）</p>
    </header>

    <div v-if="loading" class="card hint">正在加载配置…</div>
    <section v-else-if="loadError" class="card error-box">
      <p>配置加载失败：{{ loadError }}</p>
    </section>

    <template v-else>
      <section v-for="t in TIERS" :key="t.key" class="card">
        <div class="section-head">
          <h3>{{ t.label }}</h3>
          <span class="hint-inline">{{ t.hint }}</span>
        </div>
        <div class="tier-grid">
          <label class="field">
            <span class="field-label">Provider</span>
            <select v-model="form[t.key].provider" class="input">
              <option v-for="p in PROVIDER_OPTIONS" :key="p.id" :value="p.id">
                {{ p.label }}
              </option>
            </select>
          </label>
          <label class="field">
            <span class="field-label">模型名</span>
            <input
              v-model="form[t.key].model"
              class="input"
              type="text"
              placeholder="如 gpt-5.2 / claude-sonnet-4-6 / deepseek-v4-flash"
            />
          </label>
          <label class="field">
            <span class="field-label">Base URL（可选；OpenAI 兼容端点必填，官方端点亦可填中转/镜像地址）</span>
            <input
              v-model="form[t.key].base_url"
              class="input"
              type="text"
              placeholder="https://your-gateway.example.com/v1"
            />
          </label>
          <label class="field">
            <span class="field-label">API key</span>
            <span class="key-input">
              <input
                v-model="form[t.key].api_key"
                class="input"
                :type="showApiKey[t.key] ? 'text' : 'password'"
                placeholder="留空该档无法调用"
                autocomplete="new-password"
              />
              <button
                type="button"
                class="btn key-toggle"
                :aria-label="showApiKey[t.key] ? '隐藏 API key' : '显示 API key'"
                @click="showApiKey[t.key] = !showApiKey[t.key]"
              >
                {{ showApiKey[t.key] ? '隐藏' : '显示' }}
              </button>
            </span>
          </label>
        </div>
        <p class="hint">
          API key 明文存本机 ~/.corpus/config.db（ADR-0003）；留空该档无法调用。
          兼容端点将自动启用 Chat Completions 线路与 system 消息兜底。
        </p>
        <div class="tier-actions">
          <button
            type="button"
            class="btn"
            :disabled="testing[t.key]"
            @click="runTestConnection(t.key)"
          >
            {{ testing[t.key] ? '测试中…' : '测试连通' }}
          </button>
          <span v-if="testResult[t.key]" class="test-ok">
            连通成功：{{ testResult[t.key]!.latency_ms }} ms，模型回复「{{ testResult[t.key]!.reply }}」
          </span>
          <span v-else-if="testError[t.key]" class="test-err">
            {{ testError[t.key] }}
          </span>
        </div>
      </section>

      <section class="card">
        <h3>行为</h3>
        <div class="field-row">
          <span class="field-label">Apply 模式（Draft 落盘方式）</span>
          <div class="radio-group">
            <label class="radio">
              <input v-model="form.apply_mode" type="radio" value="manual" />
              人审触发（默认）
            </label>
            <label class="radio">
              <input v-model="form.apply_mode" type="radio" value="auto" />
              自动执行
            </label>
          </div>
        </div>
        <label class="checkbox-row">
          <input v-model="form.query_pages" type="checkbox" />
          Query 沉淀：把每次查询沉淀为 Query page（默认开启）
        </label>
      </section>

      <div class="actions">
        <span v-if="saveState === 'saved'" class="saved-hint">已保存</span>
        <span v-else-if="saveError" class="test-err">{{ saveError }}</span>
        <button
          type="button"
          class="btn primary"
          :disabled="saveState === 'saving'"
          @click="saveConfig"
        >
          {{ saveState === 'saving' ? '保存中…' : '保存配置' }}
        </button>
      </div>
    </template>
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

.card {
  background: var(--surface);
  border: 1px solid var(--border);
  border-radius: 10px;
  padding: 20px;
}

.card h3 {
  font-size: 15px;
}

.section-head {
  display: flex;
  align-items: baseline;
  gap: 12px;
  margin-bottom: 14px;
  flex-wrap: wrap;
}

.hint-inline {
  font-size: 12px;
  color: var(--text-muted);
}

.tier-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 14px;
}

@media (max-width: 720px) {
  .tier-grid {
    grid-template-columns: 1fr;
  }
}

.field {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

/* API key 密码输入 + 显隐切换按钮的组合行 */
.key-input {
  display: flex;
  gap: 8px;
}

.key-input .input {
  flex: 1;
  min-width: 0;
}

.key-toggle {
  flex-shrink: 0;
  padding: 8px 12px;
  font-size: 13px;
}

.field-label {
  font-size: 13px;
  color: var(--text-muted);
}

.input {
  padding: 8px 10px;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--bg);
  color: var(--text-strong);
  font-size: 14px;
  font-family: var(--mono);
}

.input:focus-visible {
  outline: 2px solid var(--accent);
  outline-offset: 1px;
}

.hint {
  margin-top: 12px;
  font-size: 12px;
  color: var(--text-muted);
}

.tier-actions {
  margin-top: 14px;
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.test-ok {
  font-size: 13px;
  color: var(--ok);
  word-break: break-all;
}

.test-err {
  font-size: 13px;
  color: var(--danger);
  word-break: break-all;
}

.error-box {
  border-color: var(--danger-border);
  background: var(--danger-soft);
  color: var(--danger);
  font-size: 14px;
}

.field-row {
  display: flex;
  align-items: center;
  gap: 16px;
  flex-wrap: wrap;
}

.radio-group {
  display: flex;
  gap: 16px;
}

.radio {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 14px;
  cursor: pointer;
}

.checkbox-row {
  margin-top: 14px;
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 14px;
  cursor: pointer;
}

.actions {
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 12px;
}

.saved-hint {
  font-size: 13px;
  color: var(--ok);
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
</style>
