use crate::progress::parse_progress;
use layer_engine_api::{EngineError, EngineEvent, FailureReason, MediaResolver, MediaTaskSpec};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

pub struct YtdlpResolver {
    program: PathBuf,
    ffmpeg_dir: Option<PathBuf>,
    /// `node:C:\...\node.exe` — YouTube imzası JS runtime ister.
    js_runtime: Option<String>,
}

impl YtdlpResolver {
    pub fn new(program: PathBuf, ffmpeg: Option<PathBuf>, node: Option<PathBuf>) -> Self {
        let ffmpeg_dir = ffmpeg
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let js_runtime = node.map(|p| format!("node:{}", p.display()));
        Self {
            program,
            ffmpeg_dir,
            js_runtime,
        }
    }
}

#[async_trait::async_trait]
impl MediaResolver for YtdlpResolver {
    async fn download(
        &self,
        spec: MediaTaskSpec,
        events: tokio::sync::mpsc::UnboundedSender<EngineEvent>,
        cancel: CancellationToken,
    ) -> Result<(), EngineError> {
        std::fs::create_dir_all(&spec.dest_dir)?;
        let out_tmpl = spec
            .dest_dir
            .join("%(title).180B [%(id)s].%(ext)s");
        let path_file = spec.dest_dir.join(format!("{}.path", spec.task_id));
        let _ = std::fs::remove_file(&path_file);

        let mut cmd = Command::new(&self.program);
        cmd.arg("--no-playlist")
            .arg("--no-colors")
            .arg("--newline")
            .arg("--no-mtime")
            .arg("--restrict-filenames")
            .arg("-o")
            .arg(&out_tmpl)
            .arg("--progress-template")
            .arg("download:%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.speed)s")
            .arg("--print-to-file")
            .arg("after_move:filepath")
            .arg(&path_file);
        if let Some(runtime) = &self.js_runtime {
            cmd.arg("--js-runtimes").arg(runtime);
        }
        if let Some(dir) = &self.ffmpeg_dir {
            cmd.arg("--ffmpeg-location")
                .arg(dir)
                .arg("-f")
                .arg("bv*+ba/b")
                .arg("--merge-output-format")
                .arg("mp4");
        } else {
            // ffmpeg yok: birleşik dosya. YouTube'da bu genelde 360p.
            cmd.arg("-f").arg("b");
        }
        cmd.arg(&spec.url)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| EngineError::Internal(e.to_string()))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let err_task = tokio::spawn(async move {
            let Some(stderr) = stderr else { return String::new() };
            let mut lines = BufReader::new(stderr).lines();
            let mut kept = String::new();
            while let Ok(Some(line)) = lines.next_line().await {
                if kept.len() < 4000 {
                    kept.push_str(&line);
                    kept.push('\n');
                }
            }
            kept
        });

        let read = async {
            if let Some(stdout) = stdout {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(progress) = parse_progress(&line) {
                        let _ = events.send(EngineEvent::Progress(progress));
                    }
                }
            }
        };

        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Ok(());
            }
            _ = read => {}
        }

        let status = child.wait().await.map_err(|e| EngineError::Internal(e.to_string()))?;
        let stderr = err_task.await.unwrap_or_default();
        if cancel.is_cancelled() {
            return Ok(());
        }
        if !status.success() {
            let _ = events.send(EngineEvent::Failed(FailureReason::Resolver(tail(&stderr))));
            return Ok(());
        }
        match finished_file(&spec.dest_dir, &spec.task_id, &path_file) {
            Ok(path) => {
                let name = path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_else(|| "video".into());
                let _ = events.send(EngineEvent::Named { file_name: name });
                let _ = events.send(EngineEvent::Done);
            }
            Err(reason) => {
                let _ = events.send(EngineEvent::Failed(reason));
            }
        }
        Ok(())
    }
}

fn tail(s: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        "yt-dlp başarısız".into()
    } else if t.len() > 500 {
        t[t.len() - 500..].to_string()
    } else {
        t.to_string()
    }
}

fn finished_file(dir: &Path, task_id: &str, path_file: &Path) -> Result<PathBuf, FailureReason> {
    if let Ok(text) = std::fs::read_to_string(path_file) {
        let path = PathBuf::from(text.trim());
        if path.is_file() {
            let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if len == 0 {
                return Err(FailureReason::Resolver("çıktı 0 bayt".into()));
            }
            let _ = std::fs::remove_file(path_file);
            return Ok(path);
        }
    }
    let mut found = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(task_id) && !name.ends_with(".path") && !name.ends_with(".part") {
                found = Some(entry.path());
            }
        }
    }
    match found {
        Some(path) if std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0) > 0 => Ok(path),
        Some(_) => Err(FailureReason::Resolver("çıktı 0 bayt".into())),
        None => Err(FailureReason::Resolver("çıktı dosyası yok".into())),
    }
}
