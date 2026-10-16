//! git 操作：shell out 到系统 git（避免引入 git2 重依赖）。

use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// 在 `repo` 目录内执行一次 git 子命令。
///
/// 非零退出与无法启动进程都转为 [`Error::Git`]。
fn run_git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|e| Error::Git {
            command: args.join(" "),
            stderr: format!("无法启动系统 git：{e}"),
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(Error::Git {
            command: args.join(" "),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// 先按 `primary` 执行 git 子命令，失败则回退到 `fallback` 重试一次。
///
/// 两次都失败时返回 `fallback` 的错误：`primary` 的失败多为能力不支持
/// （旧版本 git、未配置提交身份），属预期回退；`fallback` 都失败才说明
/// 命令真正有问题。
fn run_git_with_fallback(repo: &Path, primary: &[&str], fallback: &[&str]) -> Result<String> {
    run_git(repo, primary).or_else(|_| run_git(repo, fallback))
}

/// 提交身份回退参数（`git -c` 前缀）：全局配置缺提交身份时用 Corpus
/// 引擎身份，保证全新机器也能完成提交。两处调用（init / commit_files）
/// 共用，勿各自内联。
const CORPUS_IDENTITY_ARGS: [&str; 4] = [
    "-c",
    "user.name=Corpus",
    "-c",
    "user.email=corpus@localhost",
];

/// `git commit --message <message>`：优先沿用用户全局身份，失败回退
/// Corpus 身份重试一次（见 [`run_git_with_fallback`]）。
fn commit_with_identity_fallback(repo: &Path, message: &str) -> Result<()> {
    let fallback: Vec<&str> = CORPUS_IDENTITY_ARGS
        .iter()
        .copied()
        .chain(["commit", "--message", message])
        .collect();
    run_git_with_fallback(repo, &["commit", "--message", message], &fallback)?;
    Ok(())
}

/// `git init` + 把当前全部内容作为初始提交。
///
/// 提交身份优先沿用用户全局 git 配置；未配置时回退到 Corpus 引擎身份，
/// 保证全新机器上也能完成初始提交。
pub(super) fn init_with_initial_commit(repo: &Path, message: &str) -> Result<()> {
    // git 2.28+ 支持指定初始分支名；旧版本回退到默认分支。
    run_git_with_fallback(repo, &["init", "--branch", "main"], &["init"])?;
    run_git(repo, &["add", "--all"])?;
    commit_with_identity_fallback(repo, message)
}

/// `git add <files>` + 单次提交，返回新提交的短 hash。
///
/// 只 add 显式给出的文件（而非 `--all`）：指纹缓存等 `.corpus/` 下的
/// 可再生数据即使未被 .gitignore 覆盖也不会混入提交。每次 Apply 恰好
/// 产生一个提交（Ingest 事务语义，见票据 T3）。
pub(crate) fn commit_files(repo: &Path, files: &[String], message: &str) -> Result<String> {
    let mut add_args = vec!["add".to_string()];
    add_args.extend(files.iter().cloned());
    let add_args: Vec<&str> = add_args.iter().map(String::as_str).collect();
    run_git(repo, &add_args)?;
    commit_with_identity_fallback(repo, message)?;
    run_git(repo, &["rev-parse", "--short", "HEAD"])
}

/// `git reset`（mixed）：清空暂存区、索引回到 HEAD，工作区不动。
///
/// Apply 事务的 commit 失败分支用——`git add` 可能已把文件写入索引，
/// 恢复工作区快照后必须再清索引，工作区才算真正回到提交点。
pub(crate) fn reset_index(repo: &Path) -> Result<()> {
    run_git(repo, &["reset"])?;
    Ok(())
}
