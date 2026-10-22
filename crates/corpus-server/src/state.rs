//! 服务状态：全局 [`ConfigStore`]（与桌面壳同一应用数据目录）、摄取互斥
//! 与进度事件广播通道。

use std::sync::Arc;

use corpus_engine::ConfigStore;
use tokio::sync::{broadcast, Mutex};

/// 事件广播通道容量：进度类事件低频（引擎侧已节流），256 足够；订阅者
/// 落后即丢弃（Lagged），进度条语义可容忍，绝不反压事件生产方主流程。
const PROGRESS_CHANNEL_CAPACITY: usize = 256;

/// axum State：处理器共享的只读句柄集合。
#[derive(Clone)]
pub struct AppState {
    /// 全局配置库（sqlite，config.db），与桌面壳同一应用数据目录。
    pub store: Arc<ConfigStore>,
    /// 全局摄取互斥：同一时刻只允许一个摄取（web 多标签语义新增的
    /// 限制，桌面壳单窗口天然串行无此约束，见 POST /api/ingest 注释）。
    pub ingest_lock: Arc<Mutex<()>>,
    /// 事件广播源：/api/events 的每个 SSE 连接 subscribe 一个 receiver。
    /// 通道承载「事件名 + JSON payload」对——SSE 写循环按事件名输出
    /// `event: <名>`，新增事件类型（如 T9 查询流式）即发送新的名字对，
    /// 通道与写循环零改动（票据 #17 范围重定）。
    pub progress_tx: broadcast::Sender<(String, serde_json::Value)>,
}

impl AppState {
    pub fn new(store: ConfigStore) -> Self {
        let (progress_tx, _) = broadcast::channel(PROGRESS_CHANNEL_CAPACITY);
        Self {
            store: Arc::new(store),
            ingest_lock: Arc::new(Mutex::new(())),
            progress_tx,
        }
    }
}
