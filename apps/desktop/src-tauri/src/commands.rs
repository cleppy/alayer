use crate::view::{to_view, TaskView};
use crate::Tools;
use layer_core::{classify_url, TaskManager};
use layer_engine_api::{IntakeSource, TaskKind};
use layer_engine_bridge::probe_media;
use serde::Serialize;
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

#[derive(Debug, Serialize)]
pub struct ChoiceView {
    pub id: String,
    pub label: String,
    pub group: String,
    pub format: String,
    pub audio_format: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Prepare {
    Direct,
    Choose {
        title: String,
        choices: Vec<ChoiceView>,
    },
}

#[tauri::command]
pub async fn prepare_download(tools: State<'_, Tools>, url: String) -> Result<Prepare, String> {
    let url = url.trim().to_string();
    match classify_url(&url) {
        Some(TaskKind::Media) => {
            let ytdlp = tools
                .ytdlp
                .as_ref()
                .ok_or("yt-dlp yok, kalite seçilemez")?;
            let (title, choices) = probe_media(ytdlp, tools.node.as_deref(), &url).await?;
            Ok(Prepare::Choose {
                title,
                choices: choices
                    .into_iter()
                    .map(|c| ChoiceView {
                        id: c.id,
                        label: c.label,
                        group: c.group,
                        format: c.format,
                        audio_format: c.audio_format,
                    })
                    .collect(),
            })
        }
        Some(_) => Ok(Prepare::Direct),
        None => Err(format!("desteklenmeyen adres: {url}")),
    }
}

#[tauri::command]
pub async fn add_media(
    app: AppHandle,
    state: State<'_, TaskManager>,
    url: String,
    title: String,
    format: String,
    audio_format: Option<String>,
) -> Result<String, String> {
    let dir = downloads(&app)?;
    state
        .intake_media(url, dir, title, format, audio_format)
        .await
        .map_err(|e| e.to_string())
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
