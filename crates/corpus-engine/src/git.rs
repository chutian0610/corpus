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

/// `git init` + 把当前全部内容作为初始提交。
///
/// 提交身份优先沿用用户全局 git 配置；未配置时回退到 Corpus 引擎身份，
/// 保证全新机器上也能完成初始提交。
pub(super) fn init_with_initial_commit(repo: &Path, message: &str) -> Result<()> {
    // git 2.28+ 支持指定初始分支名；旧版本回退到默认分支。
    run_git_with_fallback(repo, &["init", "--branch", "main"], &["init"])?;
    run_git(repo, &["add", "--all"])?;
    run_git_with_fallback(
        repo,
        &["commit", "--message", message],
        &[
            "-c",
            "user.name=Corpus",
            "-c",
            "user.email=corpus@localhost",
            "commit",
            "--message",
            message,
        ],
    )?;
    Ok(())
}
