//! 翻译工作流：初始化、翻译、润色与重译的完整流程。
//!
//! CLI 与桌面端共用这些入口；CLI 只负责参数解析和终端输出，界面负责进度事件与展示。

mod common;
mod extraction;
mod init;
mod polish;
mod retranslate;
mod titles;
mod transit;

pub use init::{import_project, initialize_project};
pub use polish::polish_project;
pub use retranslate::{retranslate_project, RetranslationProgress};
pub use transit::transit_project;

pub(crate) use polish::run_polish;
pub(crate) use transit::transit;

#[cfg(test)]
pub(crate) use retranslate::{mark_retranslation_error, retranslate_item};
#[cfg(test)]
pub(crate) use transit::run_transit;
