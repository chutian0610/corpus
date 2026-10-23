/** 应用级「当前 Vault」状态：两级应用壳（启动页 + 工作区）的前端单一持有处。
 *
 * T-UX 起「打开」有了真实语义：引擎校验（open_vault）通过才进入工作区，
 * currentVault 由 null ⇄ 有值 切换启动页与工作区两级界面。
 * 打开/关闭动作收敛在此，启动页（列表/对话框）、新建流程与启动恢复
 * 共用同一入口，避免各页面自行拼装。模块级响应式单例是最小实现，
 * 无需引入状态库。 */
import { ref } from 'vue'
import { listPages as listPagesApi, openVault as openVaultApi, type PageTree, type OpenedVault } from './api'
import { errorMessage } from './display'

export interface CurrentVault {
  /** Vault 根目录。 */
  path: string
  /** Vault 绑定的模板 id。 */
  templateId: string
  /** 模板展示名（引擎下发的 TemplateInfo.name，如「Research」）。 */
  templateName: string
}

/** 当前打开的 Vault；null = 启动页。 */
export const currentVault = ref<CurrentVault | null>(null)

/** 页面树版本号：Apply 落盘后 bump，页面树消费方据此重拉。 */
export const pagesVersion = ref(0)

export function bumpPages(): void {
  pagesVersion.value++
}

/** 当前 Vault 的 wiki/ 页面树（T4 起共享：FilePanel 展示 + PagePreview
 *  wikilink 解析共用同一份，避免两处各拉一遍）。null = 未加载。 */
export const pageTree = ref<PageTree | null>(null)
export const pageTreeLoading = ref(false)
export const pageTreeError = ref<string | null>(null)

/** 拉取当前 Vault 的页面树（file 面板挂载 / 切 Vault / pagesVersion bump
 *  时调用）。Vault 未打开时清空树。 */
export async function reloadPageTree(): Promise<void> {
  const vault = currentVault.value
  if (!vault) {
    pageTree.value = null
    pageTreeError.value = null
    return
  }
  pageTreeLoading.value = true
  pageTreeError.value = null
  try {
    pageTree.value = await listPagesApi(vault.path)
  } catch (err) {
    pageTreeError.value = errorMessage(err)
    pageTree.value = null
  } finally {
    pageTreeLoading.value = false
  }
}

/** 打开一个 Vault：引擎校验 + 最近列表登记，成功后设为当前工作区。
 *
 * 校验失败（非 Vault / 模板 id 未知）向调用方抛错——由各入口决定
 * 如何展示（启动页内联报错、启动恢复静默回启动页）。 */
export async function openVault(path: string): Promise<CurrentVault> {
  const opened: OpenedVault = await openVaultApi(path)
  const vault: CurrentVault = {
    path: opened.root,
    templateId: opened.template_id,
    templateName: opened.template_name,
  }
  currentVault.value = vault
  return vault
}

/** 关闭当前 Vault：回到启动页。最近列表不动（下次仍可快速打开）。 */
export function closeVault(): void {
  currentVault.value = null
}
