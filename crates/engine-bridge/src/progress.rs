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

/// `[download] 10.3% of 45.67MiB at 8.70MiB/s`
pub fn parse_human(line: &str) -> Option<Progress> {
    let lower = line.to_ascii_lowercase();
    let of = lower.find(" of ")?;
    let at = lower[of + 4..].find(" at ").map(|i| of + 4 + i);
    let size_src = match at {
        Some(end) => &line[of + 4..end],
        None => return None,
    };
    let total = parse_size(size_src.trim())?;
    let pct = line[..of]
        .split_whitespace()
        .last()
        .and_then(|s| s.trim_end_matches('%').parse::<f64>().ok())
        .unwrap_or(0.0);
    let downloaded = ((pct / 100.0) * total as f64) as u64;
    let speed = at
        .and_then(|i| line[i + 4..].split_whitespace().next())
        .and_then(parse_size)
        .unwrap_or(0) as f64;
    Some(Progress {
        downloaded_bytes: downloaded,
        total_bytes: Some(total),
        speed_bps: speed,
    })
}

fn parse_size(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let split = raw.find(|c: char| c.is_ascii_alphabetic())?;
    let num: f64 = raw[..split].trim().parse().ok()?;
    let unit = raw[split..].trim().to_ascii_lowercase();
    let mul = match unit.as_str() {
        "b" => 1.0,
        "kib" | "kb" => 1024.0,
        "mib" | "mb" => 1024.0 * 1024.0,
        "gib" | "gb" => 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((num * mul) as u64)
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
    fn parses_human_mib_line() {
        let p = parse_human("[download] 10.0% of 10.00MiB at 1.00MiB/s").unwrap();
        assert_eq!(p.total_bytes, Some(10 * 1024 * 1024));
        assert!(p.downloaded_bytes > 0);
    }

    #[test]
    fn ignores_chatter() {
        assert!(parse_progress("[youtube] Extracting URL").is_none());
        assert!(parse_progress("download:NA|NA|NA").is_none());
    }
}
