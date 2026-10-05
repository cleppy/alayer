use crate::view::{to_view, TaskView};
use crate::Tools;
use layer_core::{classify_url, TaskManager};
use layer_engine_api::{IntakeSource, TaskKind};
use layer_engine_bridge::{probe_media, Aria2Engine};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
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
    pub bytes: Option<u64>,
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
            let ytdlp = tools.ytdlp.as_ref().ok_or("yt-dlp is missing")?;
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
                        bytes: c.bytes,
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
    expected_bytes: Option<u64>,
) -> Result<String, String> {
    let dir = downloads(&app)?;
    state
        .intake_media(url, dir, title, format, audio_format, expected_bytes)
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
pub fn open_downloads(app: AppHandle) -> Result<(), String> {
    let dir = downloads(&app)?;
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn confirm_torrent_files(
    state: State<'_, TaskManager>,
    aria: State<'_, Option<Arc<Aria2Engine>>>,
    id: String,
    indices: Vec<u32>,
) -> Result<(), String> {
    if indices.is_empty() {
        return Err("Select at least one file.".into());
    }
    state
        .remember_torrent_files(&id, &indices)
        .map_err(|e| e.to_string())?;
    let Some(aria) = aria.as_ref() else {
        return Err("aria2 is missing".into());
    };
    if aria.submit_files(&id, indices) {
        Ok(())
    } else {
        Err("file picker is no longer waiting".into())
    }
}

#[tauri::command]
pub fn global_limit_mb(state: State<'_, TaskManager>) -> f64 {
    state
        .global_limit_bps()
        .map(|bps| bps as f64 / (1024.0 * 1024.0))
        .unwrap_or(0.0)
}

#[tauri::command]
pub async fn set_global_limit_mb(
    state: State<'_, TaskManager>,
    aria: State<'_, Option<Arc<Aria2Engine>>>,
    mbps: f64,
) -> Result<(), String> {
    let bps = if mbps <= 0.0 {
        None
    } else {
        Some((mbps * 1024.0 * 1024.0) as u64)
    };
    state.set_global_limit(bps);
    if let Some(aria) = aria.as_ref() {
        aria.set_global_limit(bps).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn set_task_limit_mb(
    state: State<'_, TaskManager>,
    aria: State<'_, Option<Arc<Aria2Engine>>>,
    id: String,
    mbps: f64,
) -> Result<(), String> {
    let bps = if mbps <= 0.0 {
        None
    } else {
        Some((mbps * 1024.0 * 1024.0) as u64)
    };
    state
        .set_task_speed_limit(&id, bps)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(aria) = aria.as_ref() {
        aria.set_task_limit(&id, bps).await;
    }
    Ok(())
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
