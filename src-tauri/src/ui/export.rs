//! 导出命令。

use crate::config;
use crate::export::{self, ExportFormat, ExportOptions};
use crate::state;
use std::path::PathBuf;

/// 按格式导出项目，返回输出文件路径。
#[tauri::command]
pub fn ui_export(
    project_id: String,
    format: String,
    options: Option<ExportOptions>,
) -> Result<String, String> {
    let options = options.unwrap_or_default();
    options.validate()?;
    let loaded = config::load(None)?;
    let project = state::load_project(&loaded.state_dir, &project_id)?;
    let input = PathBuf::from(&project.source_path);
    let format = match format.as_str() {
        "txt" => ExportFormat::Txt,
        "epub" => ExportFormat::Epub,
        _ => return Err("export format must be txt or epub".to_string()),
    };
    let output = export::default_output_path(&input, format, options);
    let snapshot = state::load_export_snapshot(&loaded.state_dir, &input)?;
    let bytes = match format {
        ExportFormat::Txt => export::render_txt(&snapshot, options)?.into_bytes(),
        ExportFormat::Epub => export::render_epub(&snapshot, options)?,
    };
    export::write_atomic(&output, &bytes)?;
    state::append_log(
        &loaded.state_dir,
        &project,
        "exported",
        serde_json::json!({ "format": format.extension(), "output": output, "options": options }),
    )?;
    Ok(output.to_string_lossy().into_owned())
}
