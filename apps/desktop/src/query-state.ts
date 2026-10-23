/** 应用级查询运行状态（模块级响应式单例，与 ingest-state 同模式）。
 *
 * 为什么在模块层：查询是**应用级操作**而非页面级——后台的词法 + LLM
 * 管道不随面板切换而暂停，进度订阅（Tauri listen / web EventSource）
 * 与运行状态必须常驻，否则切到文件预览再切回，进行中的流式进度就丢了
 * （ingest #21 的同款教训）。QueryPage 只是这份状态的视图。
 *
 * 会话内历史（本次打开应用以来的问答列表）也在这里：内存即可，
 * 切换 Vault 语义上属于跨知识库提问，故 closeVault 时清空。 */
import { ref } from 'vue'
import { onQueryProgress, queryAsk, type QueryCitation } from './api'
import { errorMessage } from './display'
import { bumpPages } from './vault-state'

/** 查询阶段（引擎 QueryEvent 的 stage）。 */
export type QueryPhase = 'idle' | 'searching' | 'answering' | 'done' | 'error'

/** 一轮完成的问答（历史条目）。 */
export interface QueryTurn {
  question: string
  answer: string
  citations: QueryCitation[]
  /** 沉淀的 Query page 相对 wiki/ 路径（未沉淀为 null）。 */
  queryPagePath: string | null
}

// ---- 运行状态（应用级常驻） ----

/** 是否有查询在后台运行（跨面板持续）。 */
export const asking = ref(false)
export const phase = ref<QueryPhase>('idle')
/** searching 阶段的词法词项（引擎下发，展示用）。 */
export const searchTerms = ref<string[]>([])
/** answering 阶段已接收字符数（引擎流式 chars，已节流）。 */
export const answerChars = ref(0)
/** answering 阶段的累积回答文本（引擎随事件下发，节流快照，单调
 *  增长）——回答区流式渲染的数据源；完成后完整回答走历史条目。 */
export const answerText = ref('')
/** 最近一次查询的错误（null = 无）。 */
export const queryError = ref<string | null>(null)
/** 会话内问答历史（本次打开应用以来）。 */
export const history = ref<QueryTurn[]>([])

// ---- 进度订阅：应用级一次，常驻 ----

let listening = false

function ensureProgressListener(): void {
  if (listening) return
  listening = true
  void onQueryProgress((progress) => {
    if (!asking.value) return
    if (progress.stage === 'searching') {
      phase.value = 'searching'
      searchTerms.value = progress.terms
    } else if (progress.stage === 'answering') {
      phase.value = 'answering'
      // 同阶段 chars / 累积文本单调递增（重连等异常下也不回退）
      if (progress.chars > answerChars.value) answerChars.value = progress.chars
      if (progress.answer.length >= answerText.value.length) {
        answerText.value = progress.answer
      }
    }
    // done：结果经 queryAsk 的 Promise 到达，事件只作阶段收尾信号
  })
}

/** 运行一次查询（对当前打开的 Vault）。调用前由视图保证可提问。 */
export async function askQuestion(vaultPath: string, question: string): Promise<void> {
  const trimmed = question.trim()
  if (asking.value || trimmed === '') return
  asking.value = true
  phase.value = 'searching'
  searchTerms.value = []
  answerChars.value = 0
  answerText.value = ''
  queryError.value = null
  ensureProgressListener()
  try {
    const outcome = await queryAsk(vaultPath, trimmed)
    phase.value = 'done'
    history.value = [
      ...history.value,
      {
        question: outcome.question,
        answer: outcome.answer,
        citations: outcome.citations,
        queryPagePath: outcome.query_page_path,
      },
    ]
    // 落盘成功：Wiki 长出 Query page，通知文件面板重拉页面树
    if (outcome.query_page_path !== null) bumpPages()
  } catch (err) {
    queryError.value = errorMessage(err)
    phase.value = 'error'
  } finally {
    asking.value = false
  }
}

/** 切换 Vault 时清空会话历史与运行态（跨知识库的问答不混在一起）。
 *  由 App 的 switchVault 调用；查询运行中不请空（结果仍会落到历史）。 */
export function resetQuerySession(): void {
  if (asking.value) return
  phase.value = 'idle'
  history.value = []
  searchTerms.value = []
  answerChars.value = 0
  answerText.value = ''
  queryError.value = null
}
