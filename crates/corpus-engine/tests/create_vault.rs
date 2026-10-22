//! create_vault 集成测试：只经引擎公共 API 驱动，断言落在真实文件系统
//! （目录树、文件内容、git log）。LLM 接缝用 fake provider。
//! 共享工具收敛在 `tests/common/mod.rs`（Rust 集成测试惯例）。

mod common;

use std::fs;

use common::{fresh_vault_path, git, FakeProvider};
use corpus_engine::{
    builtin_template_infos, builtin_templates, create_vault, template_by_id, Engine, Error,
    TemplateId, TemplateInfo,
};

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
        assert!(
            root.join(".corpus/.gitignore").is_file(),
            "{}: 缺 .corpus/.gitignore",
            template.id
        );
        assert!(
            root.join(".corpus/vault.json").is_file(),
            "{}: 缺 .corpus/vault.json",
            template.id
        );

        // .corpus/.gitignore 忽略指纹缓存 state.db、LLM 调用统计
        // llm-calls.log 与 LLM debug 日志（可再生 / 本地诊断数据，不入库）
        assert_eq!(
            fs::read_to_string(root.join(".corpus/.gitignore")).unwrap(),
            "state.db\nllm-calls.log\nllm-debug.log\nllm-debug.log.1\n"
        );

        // vault.json 记录模板绑定，且可经公共 API 读回
        assert_eq!(
            corpus_engine::vault_template_id(&root).unwrap(),
            template.id
        );

        // 返回值与所选模板一致
        assert_eq!(info.root, root);
        assert_eq!(info.template_id, template.id);
        assert_eq!(info.template_name, template.name);
        assert_eq!(info.created_paths.len(), 10);
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
        ".corpus/.gitignore",
        ".corpus/vault.json",
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
fn existing_empty_directory_is_accepted() {
    // 目录选择对话框只能选中已存在的目录：空目录应可作为 Vault 根
    let (_dir, root) = fresh_vault_path("vault");
    fs::create_dir_all(&root).unwrap();

    let info = create_vault(&root, &TemplateId::from("generic")).unwrap();
    assert_eq!(info.root, root);
    // 骨架与 git 初始提交与全新路径完全一致
    assert!(root.join("raw/.gitkeep").exists());
    assert!(root.join("purpose.md").exists());
    assert!(root.join("wiki/index.md").exists());
    let status = git(&root, &["status", "--porcelain"]);
    assert!(status.is_empty(), "工作区不干净：{status}");
}

#[test]
fn existing_corpus_vault_is_rejected_with_open_guidance() {
    // 目标已是 Corpus Vault：报 AlreadyAVault（引导「打开」）而非泛泛的
    // VaultPathExists，且目录保持原样
    let (_dir, root) = fresh_vault_path("vault");
    create_vault(&root, &TemplateId::from("generic")).expect("首次创建应成功");

    let err = create_vault(&root, &TemplateId::from("research")).unwrap_err();
    assert!(matches!(err, Error::AlreadyAVault { .. }), "got: {err:?}");
    let message = err.to_string();
    assert!(
        message.contains("该目录已是一个 Vault") && message.contains("请从启动页打开"),
        "错误信息须引导打开而非重复创建：{message}"
    );

    // 模板绑定不受影响（仍是首次创建时的 generic）
    assert_eq!(
        corpus_engine::vault_template_id(&root).unwrap(),
        TemplateId::from("generic")
    );
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
fn vault_template_id_rejects_missing_metadata() {
    // 缺 .corpus/vault.json 的目录不是有效 Vault：报 NotAVault 而非 panic
    let (_dir, root) = fresh_vault_path("not-a-vault");
    fs::create_dir_all(&root).unwrap();
    let err = corpus_engine::vault_template_id(&root).unwrap_err();
    assert!(matches!(err, Error::NotAVault { .. }), "got: {err:?}");
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
