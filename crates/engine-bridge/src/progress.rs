use layer_engine_api::Progress;

/// yt-dlp `--progress-template "download:%(progress.downloaded_bytes)s|..."`.
pub fn parse_progress(line: &str) -> Option<Progress> {
    let line = line.trim();
    // `download:` şablon seçicisidir, çıktıda olmayabilir.
    let rest = line.strip_prefix("download:").unwrap_or(line);
    if !rest.chars().next()?.is_ascii_digit() {
        return None;
    }
    let mut parts = rest.split('|');
    let downloaded = parts.next()?.trim().parse::<u64>().ok()?;
    let total = parts
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "NA")
        .and_then(|s| s.parse().ok());
    let speed = parts
        .next()
        .map(str::trim)
        .and_then(|s| s.parse::<f64>().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
        .unwrap_or(0.0);
    Some(Progress {
        downloaded_bytes: downloaded,
        total_bytes: total,
        speed_bps: speed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_byte_progress() {
        let p = parse_progress("download:1200|8000|400.5").unwrap();
        assert_eq!(p.downloaded_bytes, 1200);
        assert_eq!(p.total_bytes, Some(8000));
        assert!((p.speed_bps - 400.5).abs() < 0.01);
    }

    #[test]
    fn parses_ytdlp_template_body() {
        // `--progress-template download:...` satırında `download:` seçici olarak
        // yenir; stdout'a yalnız gövde düşer. Bu satır gerçek çıktıdır.
        let p = parse_progress("1024|5098855|394286.9").unwrap();
        assert_eq!(p.downloaded_bytes, 1024);
        assert_eq!(p.total_bytes, Some(5_098_855));
    }

    #[test]
    fn ignores_chatter() {
        assert!(parse_progress("[youtube] Extracting URL").is_none());
        assert!(parse_progress("download:NA|NA|NA").is_none());
    }
}
