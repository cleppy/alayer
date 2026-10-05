mod commands;
mod sink;
mod view;

use layer_core::{Db, QueuePolicy, TaskManager};
use layer_engine_bridge::{Aria2Engine, YtdlpResolver};
use layer_engine_http::ReqwestHttpEngine;
use sink::TauriSink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::{Manager, WindowEvent};

static CLOSING: AtomicBool = AtomicBool::new(false);

pub struct Tools {
    pub ytdlp: Option<PathBuf>,
    pub node: Option<PathBuf>,
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                if CLOSING.swap(true, Ordering::SeqCst) {
                    return;
                }
                api.prevent_close();
                let window = window.clone();
                tauri::async_runtime::spawn(async move {
                    let app = window.app_handle();
                    if let Some(manager) = app.try_state::<TaskManager>() {
                        manager.shutdown().await;
                    }
                    if let Some(slot) = app.try_state::<Option<Arc<Aria2Engine>>>() {
                        if let Some(aria) = slot.as_ref() {
                            aria.save_session().await;
                        }
                    }
                    let _ = window.destroy();
                });
            }
        })
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let extra = app
                .path()
                .resource_dir()
                .ok()
                .into_iter()
                .collect::<Vec<_>>();
            let db = Arc::new(Db::open(&data.join("layer.db"))?);
            let saved_limit = db
                .get_setting("global_limit_bps")
                .ok()
                .flatten()
                .and_then(|raw| raw.parse::<u64>().ok())
                .filter(|n| *n > 0);
            let sink = Arc::new(TauriSink::new(app.handle().clone()));
            let manager = TaskManager::new(
                db,
                Arc::new(ReqwestHttpEngine::new()),
                sink,
                QueuePolicy::default(),
            );
            if let Some(bps) = saved_limit {
                manager.set_global_limit(Some(bps));
            }
            let aria = find_sidecar("aria2c.exe", &extra)
                .map(|path| Arc::new(Aria2Engine::new(path, data.join("aria2"))));
            if let Some(aria) = &aria {
                manager.set_torrent_engine(aria.clone());
                if let Some(bps) = saved_limit {
                    let aria = aria.clone();
                    tauri::async_runtime::spawn(async move {
                        aria.set_global_limit(Some(bps)).await;
                    });
                }
            } else {
                eprintln!("aria2c yok: magnet ve torrent kuyrukta bekler");
            }
            if let Some(ytdlp) = find_sidecar("yt-dlp.exe", &extra) {
                let ffmpeg = find_sidecar("ffmpeg.exe", &extra);
                let node = find_node();
                manager.set_media_resolver(Arc::new(YtdlpResolver::new(ytdlp, ffmpeg, node)));
            } else {
                eprintln!("yt-dlp yok: sayfa linkleri kuyrukta bekler");
            }
            let boot = manager.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = boot.start().await {
                    eprintln!("start: {e}");
                }
            });
            app.manage(Tools {
                ytdlp: find_sidecar("yt-dlp.exe", &extra),
                node: find_node(),
            });
            app.manage(aria);
            app.manage(manager);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_tasks,
            commands::prepare_download,
            commands::add_media,
            commands::add_url,
            commands::open_downloads,
            commands::add_torrent,
            commands::pause,
            commands::resume,
            commands::cancel,
            commands::confirm_torrent_files,
            commands::set_global_limit_mb,
            commands::global_limit_mb,
            commands::set_task_limit_mb
        ])
        .run(tauri::generate_context!())
        .expect("layer penceresi açılmadı");
}

fn find_sidecar(name: &str, extra: &[PathBuf]) -> Option<PathBuf> {
    let mut dirs = Vec::new();
    for dir in extra {
        dirs.push(dir.join("sidecars").join("bin"));
        dirs.push(dir.clone());
    }
    if let Ok(dir) = std::env::var("LAYER_SIDECARS") {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(|p| p.to_path_buf());
        for _ in 0..6 {
            let Some(current) = dir else { break };
            dirs.push(current.join("sidecars").join("bin"));
            dirs.push(current.clone());
            dir = current.parent().map(|p| p.to_path_buf());
        }
    }
    dirs.into_iter()
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
}

fn find_node() -> Option<PathBuf> {
    for candidate in [
        PathBuf::from(r"C:\Program Files\nodejs\node.exe"),
        PathBuf::from(r"C:\Program Files (x86)\nodejs\node.exe"),
    ] {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
