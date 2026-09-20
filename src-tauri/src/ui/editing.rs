use crate::{
    config,
    workflow::editing::{self, EditRequest, SegmentDetail},
};

#[tauri::command]
pub fn ui_segment_history(project_id: String, segment_id: String) -> Result<SegmentDetail, String> {
    editing::read_segment(&config::load(None)?.state_dir, &project_id, &segment_id)
}

#[tauri::command]
pub fn ui_save_segment(request: EditRequest) -> Result<SegmentDetail, String> {
    editing::save_segment(&config::load(None)?.state_dir, request)
}

#[tauri::command]
pub async fn ui_preview_retranslation(
    project_id: String,
    segment_id: String,
) -> Result<SegmentDetail, String> {
    let loaded = config::load(None)?;
    editing::preview_retranslation(&loaded.state_dir, &loaded.value, &project_id, &segment_id).await
}
