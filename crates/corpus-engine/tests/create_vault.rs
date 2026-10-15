//! create_vault 集成测试：只经引擎公共 API 驱动，断言落在真实文件系统
//! （目录树、文件内容、git log）。LLM 接缝用 fake provider。

use std::fs;
use std::process::Command;
use std::sync::{Arc, Mutex};

use corpus_engine::{
    builtin_template_infos, builtin_templates, create_vault, template_by_id, Engine, Error,
    Provider, Result, TemplateId, TemplateInfo,
};

/// 记录全部 prompt 的 fake provider（scripted fake：原样回显）。
#[derive(Clone, Default)]
struct FakeProvider {
    prompts: Arc<Mutex<Vec<String>>>,
}

impl Provider for FakeProvider {
    fn complete(&self, prompt: &str) -> Result<String> {
        self.prompts.lock().unwrap().push(prompt.to_string());
        Ok(format!("fake: {prompt}"))
    }
}

/// 在临时目录下造一个尚不存在的 vault 根路径。
fn fresh_vault_path(tag: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("创建临时目录失败");
    let root = dir.path().join(tag);
    (dir, root)
}

/// 在 `repo` 内执行 git 并返回 stdout（测试断言用）。
fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("执行 git 失败");
    assert!(
        output.status.success(),
        "git {args:?} 失败：{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

#[test]
fn create_vault_creates_complete_skeleton_for_every_template() {
    for template in builtin_templates() {
        let (_dir, root) = fresh_vault_path("vault");
        let info = create_vault(&root, &template.id).expect("create_vault 应成功");

        // 目录骨架完整
        assert!(root.join("raw").is_dir(), "{}: 缺 raw/", template.id);
        assert!(
            root.join("raw/.gitkeep").is_file(),
            "{}: 缺 raw/.gitkeep",
            template.id
        );
        assert!(root.join("wiki").is_dir(), "{}: 缺 wiki/", template.id);
        assert!(
            root.join("wiki/index.md").is_file(),
            "{}: 缺 wiki/index.md",
            template.id
        );
        assert!(
            root.join("wiki/log.md").is_file(),
            "{}: 缺 wiki/log.md",
            template.id
        );
        assert!(
            root.join("purpose.md").is_file(),
            "{}: 缺 purpose.md",
            template.id
        );
        assert!(
            root.join(".corpus").is_dir(),
            "{}: 缺 .corpus/",
            template.id
        );
        assert!(
            root.join(".corpus/.gitkeep").is_file(),
            "{}: 缺 .corpus/.gitkeep",
            template.id
        );

        // 返回值与所选模板一致
        assert_eq!(info.root, root);
        assert_eq!(info.template_id, template.id);
        assert_eq!(info.template_name, template.name);
        assert_eq!(info.created_paths.len(), 8);
        assert!(info.created_paths.contains(&"raw/.gitkeep".to_string()));
        assert!(info.created_paths.contains(&".corpus/.gitkeep".to_string()));

        // purpose.md 内容来自所选模板的 purpose 模板
        let purpose = fs::read_to_string(root.join("purpose.md")).unwrap();
        assert_eq!(purpose, template.purpose_template);
    }
}

#[test]
fn vault_is_a_git_repository_with_initial_commit() {
    let (_dir, root) = fresh_vault_path("vault");
    create_vault(&root, &TemplateId::from("generic")).expect("create_vault 应成功");

    // 合法 git 仓库
    assert!(root.join(".git").exists());

    // 有且仅有一个初始提交
    let count = git(&root, &["rev-list", "--count", "HEAD"]);
    assert_eq!(count, "1");
    let subject = git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject, "corpus: init vault");

    // 骨架文件全部纳入提交，空目录经 .gitkeep 入库，工作区干净
    let tracked = git(&root, &["ls-files"]);
    for file in [
        "raw/.gitkeep",
        "wiki/index.md",
        "wiki/log.md",
        "purpose.md",
        ".corpus/.gitkeep",
    ] {
        assert!(
            tracked.lines().any(|line| line == file),
            "未被 git 跟踪：{file}"
        );
    }
    // raw/ 与 .corpus/ 存在于初始提交的内容树：克隆 / 回滚后目录结构仍在
    let tree = git(&root, &["ls-tree", "--name-only", "HEAD"]);
    for entry in ["raw", "wiki", "purpose.md", ".corpus"] {
        assert!(
            tree.lines().any(|line| line == entry),
            "HEAD 内容树缺顶层条目：{entry}"
        );
    }
    let status = git(&root, &["status", "--porcelain"]);
    assert!(status.is_empty(), "工作区不干净：{status}");
}

#[test]
fn existing_path_is_rejected_without_overwrite() {
    // 已存在的目录
    let (_dir, root) = fresh_vault_path("vault");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("sentinel.txt"), "keep").unwrap();

    let err = create_vault(&root, &TemplateId::from("generic")).unwrap_err();
    assert!(matches!(err, Error::VaultPathExists { .. }), "got: {err:?}");

    // 原内容保持原样，未生成任何骨架
    assert_eq!(
        fs::read_to_string(root.join("sentinel.txt")).unwrap(),
        "keep"
    );
    assert!(!root.join("raw").exists());
    assert!(!root.join("purpose.md").exists());
    assert!(!root.join(".git").exists());

    // 已存在的文件同样拒绝
    let (_dir2, file_root) = fresh_vault_path("occupied-file");
    let occupied = file_root.with_file_name("occupied-file");
    fs::write(&occupied, "data").unwrap();
    let err = create_vault(&occupied, &TemplateId::from("generic")).unwrap_err();
    assert!(matches!(err, Error::VaultPathExists { .. }), "got: {err:?}");
    assert_eq!(fs::read_to_string(&occupied).unwrap(), "data");
}

#[test]
fn unknown_template_is_rejected_without_creating_anything() {
    let (_dir, root) = fresh_vault_path("vault");
    let err = create_vault(&root, &TemplateId::from("no-such-template")).unwrap_err();
    assert!(matches!(err, Error::UnknownTemplate { .. }), "got: {err:?}");
    let Error::UnknownTemplate { available, .. } = err else {
        unreachable!()
    };
    assert!(
        available.contains("generic")
            && available.contains("research")
            && available.contains("reading")
    );
    assert!(!root.exists(), "未知模板时不应创建任何文件");
}

#[test]
fn log_md_starts_with_a_greppable_init_entry() {
    let (_dir, root) = fresh_vault_path("vault");
    create_vault(&root, &TemplateId::from("reading")).expect("create_vault 应成功");

    let content = fs::read_to_string(root.join("wiki/log.md")).unwrap();
    let entry = content
        .lines()
        .find(|line| line.starts_with("## ["))
        .expect("log.md 应含 ## [ 开头的记录行");

    // 严格形如 `## [YYYY-MM-DD] init | Vault`
    let rest = entry.strip_prefix("## [").unwrap();
    let (date, tail) = rest.split_once(']').unwrap();
    assert_eq!(tail, " init | Vault");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    assert_eq!(date, today, "init 记录应为今天的本地日期");
    // YYYY-MM-DD：10 位、全数字、分隔位正确
    let bytes = date.as_bytes();
    assert_eq!(bytes.len(), 10);
    assert_eq!(&date[4..5], "-");
    assert_eq!(&date[7..8], "-");
    assert!(date[..4].chars().all(|c| c.is_ascii_digit()));
    assert!(date[5..7].chars().all(|c| c.is_ascii_digit()));
    assert!(date[8..].chars().all(|c| c.is_ascii_digit()));
}

#[test]
fn engine_accepts_injected_fake_provider() {
    let fake = FakeProvider::default();
    let engine = Engine::new(Box::new(fake.clone()));

    // 注入的 provider 可达且按 scripted 行为工作
    assert_eq!(engine.provider().complete("ping").unwrap(), "fake: ping");

    // create_vault 全程不调用 LLM：调用记录仍只有上面那次接缝验证
    let (_dir, root) = fresh_vault_path("vault");
    engine
        .create_vault(&root, &TemplateId::from("research"))
        .expect("注入 fake provider 后 create_vault 应成功");
    assert_eq!(
        fake.prompts.lock().unwrap().as_slice(),
        ["ping"],
        "T1 的 create_vault 不应触发任何 LLM 调用"
    );
    assert!(root.join("purpose.md").is_file());
}

#[test]
fn builtin_templates_are_three_distinct_constants() {
    let ids: Vec<&str> = builtin_templates().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, ["generic", "research", "reading"]);

    for template in builtin_templates() {
        assert!(!template.name.is_empty());
        assert!(!template.description.is_empty());
        assert!(!template.purpose_template.is_empty());
        assert!(!template.schema_prompt.is_empty());
        // 扩展页面类型 T13 前为空
        assert!(template.extended_page_types.is_empty());
        assert_eq!(template_by_id(&template.id), Some(template));
    }
    assert_eq!(template_by_id(&TemplateId::from("nope")), None);
}

#[test]
fn template_infos_mirror_builtin_templates_for_shell_layers() {
    let infos: Vec<TemplateInfo> = builtin_template_infos();
    let templates = builtin_templates();

    // 元数据快照与内置模板一一对应（顺序、id、展示字段），是壳层的单一数据源
    assert_eq!(infos.len(), templates.len());
    for (info, template) in infos.iter().zip(templates) {
        assert_eq!(info.id, template.id);
        assert_eq!(info.name, template.name);
        assert_eq!(info.description, template.description);
    }

    // TemplateId serde 透明：JSON 中就是字符串，壳层边界不变
    let json = serde_json::to_string(&infos).unwrap();
    assert!(json.contains("\"generic\""));
    assert!(json.contains("\"research\""));
    assert!(json.contains("\"reading\""));
    let parsed: Vec<TemplateId> =
        serde_json::from_str(r#"["generic","research","reading"]"#).unwrap();
    assert_eq!(parsed[0].as_str(), "generic");
}
