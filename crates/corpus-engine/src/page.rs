//! Wiki 页面 schema：frontmatter（七必填字段 + Source 专有指针）与
//! 页面文件的渲染 / 回读。
//!
//! 核心不变量（票据 T3 / CONTEXT.md「Alias」）：每个页面的 frontmatter
//! 必含 `type` / `title` / `created` / `updated` / `tags` / `related` /
//! `sources` 七字段，且 `aliases` 至少一项——由 [`PageFrontmatter::validate`]
//! 统一断言，Apply 前强制执行，违者整单拒绝（零写入）。
//!
//! frontmatter 用 `serde_yaml_ng`（serde_yaml 的维护 fork，见 T3 调研笔记
//! 第 1 节）双向序列化；`---` 分隔与正文拼接手写，精确控制空行。
//! 生成端只回读自己生成的文件，YAML 1.1 语义风险可控。

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// 页面类型（核心三兄弟 + Query；Topic 等后续票据引入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageKind {
    /// Source page：一次原始文档摄取的一等页面（`wiki/sources/`）。
    Source,
    /// Entity：具体现实对象（`wiki/entities/`）。
    Entity,
    /// Concept：抽象概念（`wiki/concepts/`）。
    Concept,
    /// Query page：一次查询沉淀的页面（`wiki/queries/`，T9）。
    Query,
}

impl PageKind {
    /// 全部页面类型（固定顺序：Source / Entity / Concept / Query）。
    ///
    /// 遍历各类型目录的代码（如 [`crate::wiki::list_pages`]）从这里取
    /// 清单，不各自硬编码，新增类型时只改这一处。
    pub const ALL: [PageKind; 4] = [
        PageKind::Source,
        PageKind::Entity,
        PageKind::Concept,
        PageKind::Query,
    ];

    /// 单一数据源：该类型页面在 `wiki/` 下的子目录名与 `type` 字段取值
    /// （`.0` 为 dir、`.1` 为 id），dir()/as_str() 都从这里派生。
    fn parts(self) -> (&'static str, &'static str) {
        match self {
            PageKind::Source => ("sources", "source"),
            PageKind::Entity => ("entities", "entity"),
            PageKind::Concept => ("concepts", "concept"),
            PageKind::Query => ("queries", "query"),
        }
    }

    /// 该类型页面在 `wiki/` 下的子目录名。
    pub fn dir(self) -> &'static str {
        self.parts().0
    }

    /// frontmatter `type` 字段的取值（与 serde 表示一致）。
    pub fn as_str(self) -> &'static str {
        self.parts().1
    }
}

/// 页面 frontmatter：七必填字段 + Source page 专有的原文指针。
///
/// 字段声明顺序即 YAML 输出顺序（serde 保序）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PageFrontmatter {
    /// 页面类型（YAML 键为 `type`）。
    #[serde(rename = "type")]
    pub page_type: PageKind,
    /// 页面标题。
    pub title: String,
    /// 别名数组：跨语言与缩写去重的基石，必填且至少一项。
    pub aliases: Vec<String>,
    /// 标签数组（可为空）。
    pub tags: Vec<String>,
    /// 相关页面（wikilink 形式，可为空）。
    pub related: Vec<String>,
    /// 溯源：指回 Source page 的 wikilink 列表（Source page 自身为空）。
    pub sources: Vec<String>,
    /// 创建时间（RFC 3339）。
    pub created: String,
    /// 最后更新时间（RFC 3339）。
    pub updated: String,
    /// Source page 专有：raw source 在 Vault 内的相对路径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_path: Option<String>,
    /// Source page 专有：raw source 内容的 SHA-256 指纹。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

impl PageFrontmatter {
    /// 校验核心不变量：七字段齐全（构造即保证存在，此处断言非空性）。
    ///
    /// 违规即拒绝——Draft 构建与 Apply 前都调用，保证磁盘上永远只有
    /// 合法页面。
    pub fn validate(&self) -> Result<()> {
        if self.title.trim().is_empty() {
            return Err(Error::InvalidConfig("页面 title 不得为空".to_string()));
        }
        if self.aliases.is_empty() {
            return Err(Error::InvalidConfig(format!(
                "页面「{}」aliases 为空（别名必填且至少一项）",
                self.title
            )));
        }
        if self.created.trim().is_empty() || self.updated.trim().is_empty() {
            return Err(Error::InvalidConfig(format!(
                "页面「{}」created/updated 不得为空",
                self.title
            )));
        }
        Ok(())
    }
}

/// Draft 中单个页面的落盘动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageAction {
    /// 新建页面。
    Create,
    /// 精确名命中既有页面，更新之。
    Update,
}

/// Draft 中的一个页面：目标路径 + frontmatter + 正文。
#[derive(Debug, Clone, Serialize)]
pub struct PageDraft {
    /// 相对 `wiki/` 的路径（如 `entities/transformer-架构.md`）。
    pub path: String,
    /// 新建还是更新。
    pub action: PageAction,
    /// 完整 frontmatter。
    pub frontmatter: PageFrontmatter,
    /// Markdown 正文（含 `[[wikilink]]`）。
    pub body: String,
}

impl PageDraft {
    /// 渲染完整页面文件内容（`---`frontmatter`---` + 空行 + 正文）。
    pub fn render(&self) -> Result<String> {
        self.frontmatter.validate()?;
        let yaml = serde_yaml_ng::to_string(&self.frontmatter)?;
        Ok(format!("---\n{yaml}---\n\n{}\n", self.body))
    }

    /// wikilink 目标（相对 `wiki/`、去 `.md`）：同目录内唯一，路径形式
    /// 保证跨类型不歧义（Obsidian 支持嵌套路径 wikilink）。
    pub fn link(&self) -> &str {
        self.path.strip_suffix(".md").unwrap_or(&self.path)
    }
}

/// 把页面路径渲染成 Obsidian wikilink：目标与标题一致时用短形式，
/// 否则用带别名的 `[[path|title]]`。
pub fn wikilink(link: &str, title: &str) -> String {
    if link == title {
        format!("[[{link}]]")
    } else {
        format!("[[{link}|{title}]]")
    }
}

/// 分离 frontmatter 与正文；非 `---\n` 开头或找不到结束分隔时返回 `None`。
pub fn split_frontmatter(src: &str) -> Option<(String, String)> {
    let rest = src.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let yaml = rest[..end].to_string();
    let body = rest[end + 4..].trim_start_matches('\n').to_string();
    Some((yaml, body))
}

/// 解析一个既有页面文件（自己生成的格式）：frontmatter + 正文。
pub fn parse_page(src: &str) -> Result<(PageFrontmatter, String)> {
    let (yaml, body) = split_frontmatter(src)
        .ok_or_else(|| Error::InvalidConfig("页面缺少 frontmatter（--- 分隔）".to_string()))?;
    let frontmatter: PageFrontmatter = serde_yaml_ng::from_str(&yaml)?;
    frontmatter.validate()?;
    Ok((frontmatter, body))
}

/// 文件名 slug 化（中英文安全）：
/// 保留 Unicode 字母数字（含 CJK 汉字）并小写英文，空白与 `_ . - /` 转
/// `-`，其余字符（Obsidian 非法字符、标点、emoji）丢弃；压缩连续 `-`、
/// 去首尾 `-`；结果为空时回退 `untitled`。
pub fn slugify(title: &str) -> String {
    let mut out = String::new();
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if matches!(ch, ' ' | '\t' | '_' | '.' | '-' | '/') {
            out.push('-');
        }
    }
    let mut slug = String::new();
    let mut prev_dash = false;
    for ch in out.chars() {
        if ch == '-' {
            if !prev_dash && !slug.is_empty() {
                slug.push('-');
            }
            prev_dash = true;
        } else {
            slug.push(ch);
            prev_dash = false;
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "untitled".to_string()
    } else {
        slug.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_fm() -> PageFrontmatter {
        PageFrontmatter {
            page_type: PageKind::Entity,
            title: "Transformer 架构".to_string(),
            aliases: vec!["Transformer".to_string()],
            tags: vec!["deep-learning".to_string()],
            related: vec!["[[concepts/注意力机制]]".to_string()],
            sources: vec!["[[sources/paper-2017]]".to_string()],
            created: "2026-10-16T10:00:00+08:00".to_string(),
            updated: "2026-10-16T10:00:00+08:00".to_string(),
            raw_path: None,
            fingerprint: None,
        }
    }

    #[test]
    fn render_then_parse_round_trips() {
        let fm = sample_fm();
        let page = format!(
            "---\n{}---\n\n{}\n",
            serde_yaml_ng::to_string(&fm).unwrap(),
            "正文"
        );
        let (parsed, body) = parse_page(&page).expect("回读失败");
        assert_eq!(parsed, fm);
        // render 尾部拼一个换行，split 只去头部空行：body 忠实保留
        assert_eq!(body, "正文\n");
    }

    #[test]
    fn rendered_frontmatter_contains_all_seven_fields_in_order() {
        let page = PageDraft {
            path: "entities/x.md".to_string(),
            action: PageAction::Create,
            frontmatter: sample_fm(),
            body: "b".to_string(),
        }
        .render()
        .unwrap();
        let (yaml, _) = split_frontmatter(&page).expect("frontmatter 分离失败");
        let keys: Vec<&str> = yaml
            .lines()
            .filter_map(|line| line.split_once(':').map(|(k, _)| k.trim()))
            .collect();
        assert_eq!(
            keys,
            vec!["type", "title", "aliases", "tags", "related", "sources", "created", "updated"],
            "七必填字段须全部出现且 Source 专有字段不在非 Source 页面中"
        );
    }

    #[test]
    fn source_pointer_fields_serialize_only_for_source_pages() {
        let mut fm = sample_fm();
        fm.page_type = PageKind::Source;
        fm.raw_path = Some("raw/paper.md".to_string());
        fm.fingerprint = Some("abc123".to_string());
        let yaml = serde_yaml_ng::to_string(&fm).unwrap();
        assert!(yaml.contains("raw_path: raw/paper.md"));
        assert!(yaml.contains("fingerprint: abc123"));
        // 非空校验通过（Source page 的 sources 为空数组是合法的）
        fm.validate().unwrap();
    }

    #[test]
    fn validate_rejects_empty_aliases_and_title() {
        let mut fm = sample_fm();
        fm.aliases = vec![];
        assert!(fm.validate().is_err(), "aliases 空必须拒绝");
        let mut fm = sample_fm();
        fm.title = "  ".to_string();
        assert!(fm.validate().is_err(), "title 空必须拒绝");
        let mut fm = sample_fm();
        fm.created = String::new();
        assert!(fm.validate().is_err(), "created 空必须拒绝");
    }

    #[test]
    fn slugify_is_safe_for_mixed_language_titles() {
        assert_eq!(slugify("Transformer 架构"), "transformer-架构");
        assert_eq!(
            slugify("Attention Is All You Need"),
            "attention-is-all-you-need"
        );
        assert_eq!(slugify("  多余   空白  "), "多余-空白");
        assert_eq!(slugify("A/B: the *best*?"), "a-b-the-best");
        assert_eq!(slugify("《深度学习》"), "深度学习");
        assert_eq!(slugify("???"), "untitled");
        assert_eq!(slugify("v1.2 release"), "v1-2-release");
    }

    #[test]
    fn page_kind_dir_and_str_come_from_single_table() {
        // dir / as_str / serde 三者从同一 match 派生，断言互一致防漂移
        for (kind, dir, id) in [
            (PageKind::Source, "sources", "source"),
            (PageKind::Entity, "entities", "entity"),
            (PageKind::Concept, "concepts", "concept"),
            (PageKind::Query, "queries", "query"),
        ] {
            assert_eq!(kind.dir(), dir);
            assert_eq!(kind.as_str(), id);
            let yaml = serde_yaml_ng::to_string(&kind).unwrap();
            assert!(yaml.contains(id), "serde 表示须与 as_str 一致：{yaml}");
        }
    }

    #[test]
    fn wikilink_short_and_pothole_forms() {
        assert_eq!(wikilink("a-b", "a-b"), "[[a-b]]");
        assert_eq!(wikilink("a-b", "A B"), "[[a-b|A B]]");
    }

    #[test]
    fn page_draft_link_strips_md_suffix() {
        let draft = PageDraft {
            path: "sources/paper-2017.md".to_string(),
            action: PageAction::Create,
            frontmatter: sample_fm(),
            body: String::new(),
        };
        assert_eq!(draft.link(), "sources/paper-2017");
    }

    #[test]
    fn parse_page_rejects_missing_frontmatter() {
        assert!(parse_page("没有分隔线的普通 markdown").is_err());
        assert!(parse_page("---\ntitle: 只有半截").is_err());
    }
}
