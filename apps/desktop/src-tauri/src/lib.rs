mod commands;
mod sink;
mod view;

use layer_core::{Db, QueuePolicy, TaskManager};
use layer_engine_http::ReqwestHttpEngine;
use sink::TauriSink;
use std::sync::Arc;
use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .setup(|app| {
            let data = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data)?;
            let db = Arc::new(Db::open(&data.join("layer.db"))?);
            let sink = Arc::new(TauriSink::new(app.handle().clone()));
            let manager = TaskManager::new(
                db,
                Arc::new(ReqwestHttpEngine::new()),
                sink,
                QueuePolicy::default(),
            );
            let boot = manager.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = boot.start().await {
                    eprintln!("start: {e}");
                }
            });
            app.manage(manager);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_tasks,
            commands::add_url,
            commands::pause,
            commands::resume,
            commands::cancel
        ])
        .run(tauri::generate_context!())
        .expect("layer penceresi açılmadı");
}
