/** 应用级摄取运行状态（模块级响应式单例，与 vault-state 同模式）。
 *
 * 为什么在模块层：摄取是**应用级操作**而非页面级——后台的 LLM 管道
 * 不随面板切换而暂停，进度订阅（Tauri listen / web EventSource）与
 * 运行状态必须常驻，否则切到设置再切回，进行中的进度就丢了
 *（#21 的根因：状态曾是 IngestPage 组件局部 ref，卸载即销毁）。
 * IngestPage 只是这份状态的视图。 */
import { ref } from 'vue'
import { ingestFile, onIngestProgress, type IngestProgress, type IngestReport } from './api'
import { errorMessage } from './display'
import { bumpPages } from './vault-state'

export type Phase = 'idle' | IngestProgress['stage'] | 'done' | 'error'
export type StageKey = IngestProgress['stage']

/** 一个阶段的时间标记：开始时刻 + 结束时刻（进行中为 null）。 */
export interface StageMark {
  key: StageKey
  start: number
  end: number | null
}

// ---- 运行状态（应用级常驻） ----

/** 选中的源文档路径（任意位置；摄取时由命令端复制进 raw/）。 */
export const srcPath = ref<string | null>(null)
/** 是否有摄取在后台运行（跨面板持续）。 */
export const ingesting = ref(false)
export const phase = ref<Phase>('idle')
export const report = ref<IngestReport | null>(null)
export const ingestError = ref<string | null>(null)

/** 阶段时间线（按到达顺序；同阶段的 chars 更新不产生新标记）。 */
export const stageMarks = ref<StageMark[]>([])
/** 跳动的当前时刻（ticker 驱动，让耗时计算响应式刷新）。 */
export const now = ref(Date.now())
/** 当前 LLM 阶段已接收字符数（引擎流式 chars，已节流）。 */
export const llmChars = ref(0)
/** 最近一次 chars 增量到达时刻（无进展提示的计时起点）。 */
export const lastDeltaAt = ref<number | null>(null)

// ---- 进度订阅：应用级一次，常驻 ----

let listening = false

async function ensureProgressListener(): Promise<void> {
  if (listening) return
  listening = true
  await onIngestProgress((progress) => {
    if (!ingesting.value) return
    if (phase.value !== progress.stage) {
      phase.value = progress.stage
      enterStage(progress.stage)
    }
    if (progress.stage === 'analyzing' || progress.stage === 'generating') {
      if (progress.chars > llmChars.value) {
        llmChars.value = progress.chars
        lastDeltaAt.value = Date.now()
      }
    }
  })
}

/** 阶段切换：闭合上一标记、开启新标记；LLM 阶段重置字数与等待计时。 */
function enterStage(stage: StageKey): void {
  const marks = stageMarks.value
  const last = marks[marks.length - 1]
  if (last !== undefined) {
    if (last.key === stage) return // 同阶段的 chars 更新，不产生新标记
    last.end = Date.now()
  }
  marks.push({ key: stage, start: Date.now(), end: null })
  llmChars.value = 0
  lastDeltaAt.value = Date.now()
}

/** 结束时间线（完成或失败）：定格最后阶段的耗时。 */
function closeStages(): void {
  const last = stageMarks.value[stageMarks.value.length - 1]
  if (last !== undefined && last.end === null) last.end = Date.now()
}

// ---- ticker：摄取运行期间驱动耗时跳动，空闲即停 ----

let ticker: number | null = null

function startTicker(): void {
  if (ticker === null) {
    ticker = window.setInterval(() => {
      now.value = Date.now()
    }, 250)
  }
}

function stopTicker(): void {
  if (ticker !== null) {
    window.clearInterval(ticker)
    ticker = null
  }
}

// ---- 动作 ----

/** 设置源文档路径（浏览选择后调用）。 */
export function setSrcPath(path: string): void {
  srcPath.value = path
  ingestError.value = null
}

/** 运行摄取（对传入的当前 Vault）。调用前由视图保证 canIngest。 */
export async function runIngest(vaultPath: string): Promise<void> {
  if (ingesting.value || srcPath.value === null) return
  ingesting.value = true
  // 进度时间线归零：读取阶段从点击开始计时（后端首事件同 key 自动去重）
  stageMarks.value = []
  llmChars.value = 0
  lastDeltaAt.value = null
  phase.value = 'reading'
  enterStage('reading')
  report.value = null
  ingestError.value = null
  startTicker()
  void ensureProgressListener()
  try {
    report.value = await ingestFile(vaultPath, srcPath.value)
    phase.value = 'done'
    // 落盘成功：Wiki 有新页面，通知文件面板重拉页面树
    if (report.value.applied) bumpPages()
  } catch (err) {
    ingestError.value = errorMessage(err)
    phase.value = 'error'
  } finally {
    closeStages()
    ingesting.value = false
    stopTicker()
  }
}
