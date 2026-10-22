//! Corpus 本机 web 服务壳（票据 #17 v1）：corpus-engine 公共 API 的 HTTP
//! 包装。领域逻辑零份——与桌面壳同级的最薄一层壳，浏览器
//! 由此成为与桌面并列的第二壳（真后端 + Chrome DevTools 调试闭环）。
//!
//! 安全基线：仅绑 127.0.0.1；/api 一律经本机源校验（Host 必须为回环
//! 名、Origin 存在时必须为本机源，拦 drive-by CSRF 与 DNS rebinding，
//! 见 app.rs 注释）；无任何
//! CORS 放行。

mod app;
mod error;
mod state;

use std::net::{Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use corpus_engine::ConfigStore;

use crate::state::AppState;

const DEFAULT_PORT: u16 = 8619;

struct Options {
    port: u16,
    dist: Option<PathBuf>,
}

fn print_usage() {
    println!(
        "Corpus 本机 web 服务壳\n\n\
         用法：corpus-server [--port <端口>] [--dist <前端静态目录>]\n\n\
         --port      监听端口（默认 {DEFAULT_PORT}；只绑 127.0.0.1）\n\
         --dist      前端静态目录（默认依次探测 CWD 的 apps/desktop/dist\n\
                     与编译期相对路径，见 README/源码注释）\n\
         -h, --help  显示本帮助\n\n\
         环境变量：CORPUS_HOME 设置后 config.db 存 $CORPUS_HOME/（e2e
         隔离 / 沙盒实例）；未设置默认 ~/.corpus（与桌面壳共用）"
    );
}

fn parse_args() -> Result<Options, String> {
    let mut opts = Options {
        port: DEFAULT_PORT,
        dist: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print_usage();
                std::process::exit(0);
            }
            "--port" => {
                let value = args.next().ok_or("--port 需要一个端口号，如 --port 8619")?;
                opts.port = value
                    .parse()
                    .map_err(|_| format!("--port 需要合法端口号（1-65535），得到：{value}"))?;
            }
            "--dist" => {
                let value = args.next().ok_or("--dist 需要一个目录路径")?;
                opts.dist = Some(PathBuf::from(value));
            }
            other => return Err(format!("未知参数：{other}（--help 查看用法）")),
        }
    }
    Ok(opts)
}

/// `--dist` 默认取值逻辑：显式参数优先；否则依次探测——
/// ① CWD 相对的 `apps/desktop/dist`（workspace 根 `cargo run -p
///    corpus-server` 的常态，前端 `pnpm build` 产物落在这里）；
/// ② 编译期 `CARGO_MANIFEST_DIR` 相对的 `../../apps/desktop/dist`
///    （本地开发机上二进制未搬出 target/ 时的兜底）。
/// 两处都不存在则报错：提示先 `pnpm build` 或显式传 `--dist`
/// （不把编译期路径当唯一事实——二进制可能被搬到别的机器/目录）。
fn resolve_dist(explicit: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(dir) = explicit {
        return Ok(dir.to_path_buf());
    }
    let manifest_relative =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/desktop/dist");
    let candidates = [PathBuf::from("apps/desktop/dist"), manifest_relative];
    for candidate in &candidates {
        if candidate.is_dir() {
            return Ok(candidate.clone());
        }
    }
    Err(
        "未找到前端静态目录 apps/desktop/dist（CWD 相对与编译期相对均不存在）。\n\
         请先在 apps/desktop 下执行 `pnpm build`，或显式传 --dist <目录>。"
            .to_string(),
    )
}

/// 一次性访问令牌：OS 熵源（CSPRNG）的 16 字节随机数 → 32 hex 字符
/// 全局数据目录：`CORPUS_HOME` 环境变量优先（e2e / 沙盒隔离；设了即建），
/// 未设置落回 `~/.corpus`——与桌面壳同一目录、同一 config.db
/// （三平台一致的开发者工具路径）。
fn app_data_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("无法定位用户主目录（dirs::home_dir 返回 None）")?;
    let dir = corpus_engine::corpus_home(home.join(".corpus"));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("创建数据目录 {} 失败：{e}", dir.display()))?;
    Ok(dir)
}

#[tokio::main]
async fn main() {
    let opts = match parse_args() {
        Ok(opts) => opts,
        Err(err) => {
            eprintln!("corpus-server：{err}");
            std::process::exit(2);
        }
    };
    let dist = match resolve_dist(opts.dist.as_deref()) {
        Ok(dist) => dist,
        Err(err) => {
            eprintln!("corpus-server：{err}");
            std::process::exit(2);
        }
    };
    let data_dir = match app_data_dir() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("corpus-server：{err}");
            std::process::exit(1);
        }
    };
    let store = match ConfigStore::open(&data_dir) {
        Ok(store) => store,
        Err(err) => {
            eprintln!(
                "corpus-server：打开配置库失败（{}）：{err}",
                data_dir.display()
            );
            std::process::exit(1);
        }
    };
    let state = AppState::new(store);
    let router = app::build_router(state, &dist);

    // 先 bind 再打印 URL（打印出来的地址即可用）；只绑 loopback，
    // 拒绝任何非本机来源。
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, opts.port));
    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("corpus-server：绑定 {addr} 失败（端口被占用？换 --port）：{err}");
            std::process::exit(1);
        }
    };

    println!("Corpus web 壳：http://127.0.0.1:{}/", opts.port);
    println!(
        "静态目录：{}（刷新页面 404 时先 pnpm build 或检查 --dist）",
        dist.display()
    );
    let home_note = if std::env::var("CORPUS_HOME")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
    {
        "CORPUS_HOME 沙盒（与桌面壳数据隔离）"
    } else {
        "config.db 与桌面壳共用"
    };
    println!("数据目录：{}（{home_note}）", data_dir.display());
    println!("安全：仅接受本机访问（Host/Origin 校验），跨源请求一律拒绝。");

    if let Err(err) = axum::serve(listener, router).await {
        eprintln!("corpus-server：服务异常退出：{err}");
        std::process::exit(1);
    }
}
