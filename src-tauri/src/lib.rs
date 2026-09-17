//! TransItPls 核心库：桌面端与命令行共用的翻译引擎。
//!
//! 模块分工：
//! - `model`：核心数据模型；`state`：项目持久化与文件锁；
//! - `parser`：TXT/EPUB 解析；`export`：TXT/EPUB 导出；
//! - `analysis`/`terms`/`polish`/`pipeline`/`review`：分析与术语流程；
//! - `llm`：模型客户端与提示词；`workflow`：可复用的完整流程；
//! - `cli`：命令行入口；`ui`：桌面端命令层；`config`/`credentials`/`usage`：配置与记录。

pub mod analysis;
pub mod cli;
pub mod config;
pub mod credentials;
pub mod export;
pub mod llm;
pub mod model;
pub mod parser;
pub mod pipeline;
pub mod polish;
pub mod review;
pub mod schema;
pub mod state;
pub mod terms;
pub mod ui;
pub mod usage;
pub mod workflow;

/// 启动 Tauri 应用：注册任务注册表、插件与全部前端命令。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ui::TaskRegistry::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            ui::project::ui_bootstrap,
            ui::settings::ui_verify_and_save_model,
            ui::settings::ui_save_general,
            ui::settings::ui_save_pipeline,
            ui::settings::ui_save_task_config,
            ui::project::ui_project,
            ui::project::ui_delete_project,
            ui::project::ui_import,
            ui::project::ui_initialize,
            ui::project::ui_reanalyze,
            ui::translate::ui_transit,
            ui::translate::ui_polish,
            ui::translate::ui_cancel_task,
            ui::terms::ui_resolve_term,
            ui::terms::ui_set_term_policy,
            ui::terms::ui_undo_term_resolution,
            ui::terms::ui_scan_term_impact,
            ui::terms::ui_retranslate,
            ui::terms::ui_restore_translation,
            ui::export::ui_export,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
