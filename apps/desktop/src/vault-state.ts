/** 应用级「当前 Vault」状态：最近打开联动（T2）的前端单一持有处。
 *
 * T2 没有 Vault 浏览 / 工作区 UI（T4 范围）——「打开」到此为止：
 * 只记住当前 Vault（路径 + 模板）并在导航栏展示；T4 接手真正的
 * 工作区导航。模块级响应式单例是最小实现，无需引入状态库。 */
import { ref } from 'vue'

export interface CurrentVault {
  /** Vault 根目录。 */
  path: string
  /** Vault 绑定的模板 id。 */
  templateId: string
}

export const currentVault = ref<CurrentVault | null>(null)
