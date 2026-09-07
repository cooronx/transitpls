pub mod analysis;
pub mod cli;
pub mod config;
pub mod credentials;
pub mod export;
pub mod llm;
pub mod model;
pub mod parser;
pub mod pipeline;
pub mod state;
pub mod terms;
pub mod ui;
pub mod usage;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(ui::TaskRegistry::default())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            ui::ui_bootstrap,
            ui::ui_verify_and_save_model,
            ui::ui_save_general,
            ui::ui_save_pipeline,
            ui::ui_save_task_config,
            ui::ui_project,
            ui::ui_delete_project,
            ui::ui_import,
            ui::ui_initialize,
            ui::ui_reanalyze,
            ui::ui_transit,
            ui::ui_cancel_task,
            ui::ui_resolve_term,
            ui::ui_export,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
