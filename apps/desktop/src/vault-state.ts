/** 应用级「当前 Vault」状态：两级应用壳（启动页 + 工作区）的前端单一持有处。
 *
 * T-UX 起「打开」有了真实语义：引擎校验（open_vault）通过才进入工作区，
 * currentVault 由 null ⇄ 有值 切换启动页与工作区两级界面。
 * 打开/关闭动作收敛在此，启动页（列表/对话框）、新建流程与启动恢复
 * 共用同一入口，避免各页面自行拼装。模块级响应式单例是最小实现，
 * 无需引入状态库。 */
import { ref } from 'vue'
import { openVault as openVaultApi, type OpenedVault } from './api'

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

/** 页面树版本号：Apply 落盘后 bump，FilePanel 据此重拉 list_pages。 */
export const pagesVersion = ref(0)

export function bumpPages(): void {
  pagesVersion.value++
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
