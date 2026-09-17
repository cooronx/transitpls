//! 桌面端命令层：接收前端调用、复用 workflow 流程，并把状态整理成前端结构。
//!
//! 每个 `ui_*` 函数都是一个 Tauri 命令；通过 `mod.rs` 重新导出，前端调用名保持不变。

pub(crate) mod dto;
pub(crate) mod export;
pub(crate) mod project;
pub(crate) mod settings;
pub(crate) mod terms;
pub(crate) mod translate;

#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Mutex;
use tokio::task::AbortHandle;

/// 运行中任务的注册表：按任务 ID 保存中止句柄，供取消命令使用。
#[derive(Default)]
pub struct TaskRegistry {
    tasks: Mutex<HashMap<String, AbortHandle>>,
}
