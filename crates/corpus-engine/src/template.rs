//! Template 体系：Vault 创建时选定的页面类型组织方案。
//!
//! 模板是编译期 Rust 常量、不做用户自定义（见 ADR-0002）：用类型系统对
//! 页面 schema 强约束，ingest 与 lint 的口径天然一致。
//!
//! v0.1 内置三个模板：Generic / Research / Reading。
//! `schema_prompt` 与 `purpose_template` 在本票（T1）为合理初版，后续票据打磨。

/// 模板特有的扩展页面类型声明（T1 留空，T13 完善）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTypeSpec {
    /// 页面类型 id（小写单词，如 `book`）。
    pub id: &'static str,
    /// 展示名。
    pub name: &'static str,
    /// 该类型的职责一句话说明。
    pub description: &'static str,
}

/// Vault 创建时选定的页面类型组织方案（编译期常量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WikiTemplate {
    /// 模板 id（小写单词，Vault 创建时以此引用）。
    pub id: &'static str,
    /// 展示名。
    pub name: &'static str,
    /// 模板适用场景的一句话说明（选择器展示用）。
    pub description: &'static str,
    /// 注入 LLM 结构化分析步的 schema prompt：告诉模型如何把 Raw source
    /// 路由到页面类型并抽取字段。初版，后续票据打磨。
    pub schema_prompt: &'static str,
    /// 生成 Vault 根部 `purpose.md` 的模板。初版，后续票据打磨。
    pub purpose_template: &'static str,
    /// 模板特有的扩展页面类型（核心类型 entity / concept / topic / source /
    /// query 等之外的部分）。T1 为空切片，T13 完善。
    pub extended_page_types: &'static [PageTypeSpec],
}

/// 通用知识库模板：面向通用个人知识，适合大多数场景。
pub const GENERIC: WikiTemplate = WikiTemplate {
    id: "generic",
    name: "Generic",
    description: "通用知识库：以 Entity / Concept / Topic 组织日常积累，适合大多数场景。",
    schema_prompt: "You are structuring knowledge for a generic personal wiki. \
Read the raw source and route its content into pages: entities (concrete things like people, \
organizations, products), concepts (abstract ideas), and topics (discussion scopes). \
For each page decide create-vs-update by exact name or alias match, list aliases \
(including cross-language and abbreviations), and cite the source by its raw path. \
Mark in-page contradictions instead of resolving them. Respond in the JSON schema \
provided by the engine.",
    purpose_template: "# Purpose\n\n\
本 Vault 采用 **Generic** 模板：面向通用个人知识的组织方式。\n\n\
## 目标\n\n\
- （这个 Vault 为谁服务？要长期解决什么问题？）\n\n\
## 关键问题\n\n\
- （希望这个 Wiki 持续回答的核心问题）\n\n\
## 范围\n\n\
- 讨论：（列出要纳入的内容类型与主题）\n\
- 不讨论：（列出明确排除的内容）\n",
    extended_page_types: &[],
};

/// 研究模板：面向课题研究，强调问题、方法与文献脉络。
pub const RESEARCH: WikiTemplate = WikiTemplate {
    id: "research",
    name: "Research",
    description: "研究：围绕课题的问题、方法与文献脉络组织页面，便于追源与对比。",
    schema_prompt: "You are structuring knowledge for a research vault. \
Read the raw source and route its content into pages: entities (people, labs, datasets, \
tools), concepts (methods, theories), and topics framed as research questions. \
Prefer synthesizing claims with provenance to the source; capture competing results \
as marked contradictions rather than resolving them. For each page decide \
create-vs-update by exact name or alias match and cite the source by its raw path. \
Respond in the JSON schema provided by the engine.",
    purpose_template: "# Purpose\n\n\
本 Vault 采用 **Research** 模板：面向课题研究的组织方式。\n\n\
## 研究课题\n\n\
- （课题名称与一句话描述）\n\n\
## 关键问题\n\n\
- （本课题要回答的核心研究问题）\n\n\
## 方法与文献\n\n\
- （关注的方法、基准与必读文献）\n\n\
## 范围\n\n\
- 讨论：（纳入的子课题与材料类型）\n\
- 不讨论：（明确排除的方向）\n",
    extended_page_types: &[],
};

/// 阅读模板：面向长期阅读沉淀，强调书摘、人物与主题。
pub const READING: WikiTemplate = WikiTemplate {
    id: "reading",
    name: "Reading",
    description: "阅读：以书摘、作者、人物与主题沉淀长期阅读，构建个人阅读图谱。",
    schema_prompt: "You are structuring knowledge for a reading vault. \
Read the raw source and route its content into pages: entities (authors, books, \
characters, places), concepts (themes and motifs), and topics for reading threads. \
Capture notable passages as pointers into the source rather than full quotes, and \
link related works via the author and theme pages. For each page decide \
create-vs-update by exact name or alias match and cite the source by its raw path. \
Respond in the JSON schema provided by the engine.",
    purpose_template: "# Purpose\n\n\
本 Vault 采用 **Reading** 模板：面向长期阅读沉淀的组织方式。\n\n\
## 阅读主题\n\n\
- （长期跟踪的阅读主题与书单）\n\n\
## 关注的作者与作品\n\n\
- （重点作者、代表作及其脉络）\n\n\
## 范围\n\n\
- 收录：（纳入的材料类型，如书籍、文章、讲稿）\n\
- 不收录：（明确排除的材料）\n",
    extended_page_types: &[],
};

/// 内置模板全集。
pub const BUILTIN_TEMPLATES: &[WikiTemplate] = &[GENERIC, RESEARCH, READING];

/// 全部内置模板（按展示顺序）。
pub fn builtin_templates() -> &'static [WikiTemplate] {
    BUILTIN_TEMPLATES
}

/// 按 id 查找内置模板。
pub fn template_by_id(id: &str) -> Option<&'static WikiTemplate> {
    BUILTIN_TEMPLATES.iter().find(|template| template.id == id)
}
