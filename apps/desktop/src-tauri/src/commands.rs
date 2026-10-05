use crate::view::{to_view, TaskView};
use layer_core::TaskManager;
use layer_engine_api::IntakeSource;
use std::path::PathBuf;
use tauri::{AppHandle, Manager, State};

fn downloads(app: &AppHandle) -> Result<PathBuf, String> {
    let base = app
        .path()
        .download_dir()
        .map_err(|e| e.to_string())?
        .join("layer");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    Ok(base)
}

#[tauri::command]
pub async fn list_tasks(state: State<'_, TaskManager>) -> Result<Vec<TaskView>, String> {
    let tasks = state.list().map_err(|e| e.to_string())?;
    Ok(tasks.iter().map(to_view).collect())
}

#[tauri::command]
pub async fn add_url(
    app: AppHandle,
    state: State<'_, TaskManager>,
    url: String,
) -> Result<String, String> {
    let dir = downloads(&app)?;
    state
        .intake(IntakeSource::Ui, url, dir)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_torrent(
    app: AppHandle,
    state: State<'_, TaskManager>,
    path: String,
) -> Result<String, String> {
    let dir = downloads(&app)?;
    state
        .intake_torrent_file(std::path::PathBuf::from(path), dir)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn pause(state: State<'_, TaskManager>, id: String) -> Result<(), String> {
    state.pause(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resume(state: State<'_, TaskManager>, id: String) -> Result<(), String> {
    state.resume(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn cancel(state: State<'_, TaskManager>, id: String) -> Result<(), String> {
    state.cancel(&id).await.map_err(|e| e.to_string())
}
