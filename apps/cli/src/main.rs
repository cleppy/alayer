mod sink;

use clap::{Parser, Subcommand};
use layer_core::{Db, QueuePolicy, TaskManager};
use layer_engine_api::IntakeSource;
use layer_engine_http::ReqwestHttpEngine;
use sink::StdoutSink;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "layer-cli", about = "layer indirme çekirdeği (headless)")]
struct Cli {
    #[arg(long, default_value = "layer.db")]
    db: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// URL indir, bitene kadar bekle. Çıkış: 0 tamam, 1 hata.
    Add {
        url: String,
        #[arg(long, default_value = ".")]
        dir: PathBuf,
        #[arg(long)]
        limit: Option<u64>,
    },
    List,
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Cancel {
        id: String,
    },
}

fn open_db(path: &std::path::Path) -> Arc<Db> {
    Arc::new(Db::open(path).unwrap_or_else(|e| {
        eprintln!("db: {e}");
        std::process::exit(2);
    }))
}

fn manager(db: Arc<Db>, sink: Arc<StdoutSink>) -> TaskManager {
    TaskManager::new(
        db,
        Arc::new(ReqwestHttpEngine::new()),
        sink,
        QueuePolicy::default(),
    )
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let db = open_db(&cli.db);

    match cli.cmd {
        Cmd::List => {
            let tasks = db.list_tasks().unwrap_or_else(|e| {
                eprintln!("list: {e}");
                std::process::exit(2);
            });
            if tasks.is_empty() {
                println!("(boş)");
                return;
            }
            for t in tasks {
                let total = t
                    .total_bytes
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "?".into());
                println!(
                    "{}  {:?}  {:?}  {}/{total}  {}",
                    t.id, t.kind, t.state, t.downloaded_bytes, t.file_name
                );
            }
        }
        Cmd::Add { url, dir, limit } => {
            let sink = Arc::new(StdoutSink::new());
            let mgr = manager(db, sink.clone());
            mgr.set_global_limit(limit);
            if let Err(e) = mgr.start().await {
                eprintln!("start: {e}");
                std::process::exit(2);
            }
            let id = match mgr.intake(IntakeSource::Cli, url, dir).await {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("intake: {e}");
                    std::process::exit(2);
                }
            };
            println!("{id}");
            sink.watch(id);
            let wait = sink.done.notified();
            tokio::pin!(wait);
            // notify_waiters yalnız o an bekleyenleri uyandırır. missed wakeup
            // için DB poll yedeği: notified + periyodik kontrol.
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {
                        if sink.exit_ok.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                            break;
                        }
                    }
                }
            }
            mgr.shutdown().await;
            let ok = sink
                .exit_ok
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .unwrap_or(false);
            std::process::exit(if ok { 0 } else { 1 });
        }
        Cmd::Pause { id } => {
            let mgr = manager(db, Arc::new(StdoutSink::new()));
            if let Err(e) = mgr.pause(&id).await {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        Cmd::Resume { id } => {
            let sink = Arc::new(StdoutSink::new());
            let mgr = manager(db, sink.clone());
            if let Err(e) = mgr.start().await {
                eprintln!("{e}");
                std::process::exit(2);
            }
            if let Err(e) = mgr.resume(&id).await {
                eprintln!("{e}");
                std::process::exit(1);
            }
            sink.watch(id);
            let wait = sink.done.notified();
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    _ = tokio::time::sleep(Duration::from_millis(200)) => {
                        if sink.exit_ok.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
                            break;
                        }
                    }
                }
            }
            mgr.shutdown().await;
            let ok = *sink.exit_ok.lock().unwrap_or_else(|e| e.into_inner());
            std::process::exit(if ok == Some(true) { 0 } else { 1 });
        }
        Cmd::Cancel { id } => {
            let mgr = manager(db, Arc::new(StdoutSink::new()));
            if let Err(e) = mgr.cancel(&id).await {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
}
