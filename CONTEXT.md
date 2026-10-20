# Corpus

Corpus 是基于 Karpathy LLM Wiki 模式的 Rust 桌面知识库引擎：LLM 增量构建并持续维护一个持久化 Wiki，而非在每次查询时临时拼凑检索结果。

## Language

### 工作区结构

**Vault**:
一个独立的知识库：一个目录即一个 git 仓库，包含 Raw sources、Wiki 与 Purpose。一个 Vault 在创建时绑定一个 Template。
_Avoid_: project；workspace（指磁盘上的知识库本体时——「工作区」一词保留给应用壳的 Workspace 界面，见下一条）

**Workspace（应用工作区）**:
应用壳的第二级界面（T-UX）：打开某个 Vault 后的浏览与操作环境（左侧文件面板、主内容区、底部状态栏）。它只是当前打开 Vault 的 UI 上下文，不是数据单位——一个 Workspace 任一时刻至多对应一个 Vault。
_Avoid_: 与 Vault 混用（指磁盘上的知识库本体时一律说 Vault，Workspace 只指界面形态）

**Raw source**:
用户策展、引擎只读的不可变原始文档，存放于 Vault 的 `raw/` 目录。文件经 Ingest 导入（复制）进入 Vault，是策展动作；「只读」指引擎不改写 raw/ 内既有内容——导入复制是唯一入口。
_Avoid_: 原文、资料（作为术语使用时）

**Wiki**:
LLM 全权维护的 Markdown 页面目录，存放于 Vault 的 `wiki/` 目录，是持续复利的知识资产。

**Purpose**:
Vault 根部的 `purpose.md`，定义这个 Vault 的目标、关键问题与研究范围。

**Template**:
Vault 创建时选定的页面类型组织方案（扩展页面类型 + schema prompt + purpose 模板），由引擎内置，不支持用户自定义。
_Avoid_: 模板方案、schema（schema 指引擎内置的规则层时另有所指）

### 页面

**Source page**:
Wiki 中代表一次原始文档摄取的一等页面：一个 raw source 对应一个 source page，以指针（raw 路径 + 内容指纹）锚定原文，正文承载 LLM 摘要与本次产出的页面索引，是溯源的锚点。
_Avoid_: claim ledger（溯源由 source page 承担，不存在独立断言账本）

**Entity**:
Wiki 中代表一个具体现实对象（人物、组织、产品等）的页面——领域的"个体"。
_Avoid_: 实体（代码与提交信息中用 entity）

**Concept**:
Wiki 中代表一个抽象概念的页面——领域的"抽象类"，与 Entity（个体）相对。

**Topic**:
一个讨论范围：以一等页面类型存在于 `wiki/topics/`，声明边界（讨论什么、不讨论什么）并以 wikilink 索引成员页面，是 Wiki Graph 的枢纽节点。
_Avoid_: 把 Topic 当作 Concept 的同义词

**Overview**:
综述某个 Topic 范围内现有内容的页面（"说了什么"），与 Topic（"管什么"）分工。

**Synthesis**:
综合多个页面推理得出新结论的页面。

**Alias**:
页面 frontmatter 中的别名数组（`aliases`），每页必填且至少一项，是跨语言与缩写去重的基石，属核心不变量。

**Query page**:
一次查询沉淀而成的 Wiki 页面：记录问题、回答与引用过的页面。默认生成，可配置关闭。

### 操作

**Ingest**:
把一个 raw source 编译进 Wiki 的操作：产出或更新若干页面，全部经由 Draft/Apply 事务落盘。同一 raw source 重复 Ingest 不产生重复页面。
_Avoid_: 导入、索引（index 特指 index.md）

**Draft**:
一次事务中尚未落盘的变更集，可被完整检视或整体丢弃。
_Avoid_: 暂存、staging

**Apply**:
把一个 Draft 原子写入 Wiki 的操作；自动执行还是人审触发由配置决定。

**Lint**:
对 Wiki 的一致性检查，由两部分组成：确定性规则（死链、孤儿页、元数据缺失）与 LLM 矛盾检测（页内多源矛盾与页间矛盾）。

**矛盾（Contradiction）**:
同一页面内来自不同 raw source 的内容互相冲突（页内矛盾），或两个页面就同一事实给出冲突表述（页间矛盾）。矛盾只被检测与标记，不被自动裁决。

### 检索

**Wiki Graph**:
由页面间 Wikilink 构成的图，是检索与后续图分析的基础数据结构。

**PPR 检索**:
Personalized PageRank 图检索：以词法命中的页面为种子沿 Wiki Graph 扩散计算相关性。首发版本的检索方式；向量检索与混合检索属于后续版本路线图，不是被永久排除的方向。
