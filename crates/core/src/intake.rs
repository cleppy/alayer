use layer_engine_api::TaskKind;

/// URL şemasından görev türü sınıflandırması (TaskIntake'in saf karar mantığı).
///
/// M1: `http(s)` → Http, `magnet:` → Torrent. Medya ayrımı (YouTube/TikTok vb.
/// http linkinin Media sayılması) M4'te resolver host eşleme tablosuyla gelecek;
/// o zamana kadar tüm http linkleri düz HTTP görevi olarak iner — kullanıcı
/// "Link yapıştır" akışını seçerse görev Media'ya yükseltilir (M4).
pub fn classify_url(url: &str) -> Option<TaskKind> {
    let u = url.trim();
    if u.starts_with("magnet:") {
        Some(TaskKind::Torrent)
    } else if u.starts_with("http://") || u.starts_with("https://") {
        match host_of(u) {
            Some(host) if is_media_host(host) => Some(TaskKind::Media),
            Some(_) => Some(TaskKind::Http),
            None => None,
        }
    } else {
        None
    }
}

fn host_of(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let hostport = authority.rsplit('@').next()?;
    Some(hostport.split(':').next()?.trim())
}

/// Sayfa linki: yt-dlp'nin çözdüğü hostlar. Düz dosya URL'si burada değil.
fn is_media_host(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    const ROOTS: &[&str] = &[
        "youtube.com",
        "youtu.be",
        "tiktok.com",
        "twitter.com",
        "x.com",
        "instagram.com",
        "reddit.com",
        "redd.it",
        "soundcloud.com",
        "vimeo.com",
        "twitch.tv",
    ];
    ROOTS
        .iter()
        .any(|root| host == *root || host.ends_with(&format!(".{root}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_schemes() {
        assert_eq!(classify_url("https://e.com/f"), Some(TaskKind::Http));
        assert_eq!(classify_url("http://e.com/f"), Some(TaskKind::Http));
        assert_eq!(
            classify_url("magnet:?xt=urn:btih:abc"),
            Some(TaskKind::Torrent)
        );
        assert_eq!(classify_url("  https://e.com/f  "), Some(TaskKind::Http));
        assert_eq!(
            classify_url("https://youtu.be/RRuqmvda7w8"),
            Some(TaskKind::Media),
            "youtube sayfası dosya değil"
        );
        assert_eq!(
            classify_url("https://www.youtube.com/watch?v=RRuqmvda7w8"),
            Some(TaskKind::Media)
        );
        assert_eq!(
            classify_url("https://example.com/files/a.zip"),
            Some(TaskKind::Http)
        );
        assert_eq!(classify_url("ftp://e.com/f"), None);
        assert_eq!(classify_url("file:///C:/x"), None);
        assert_eq!(classify_url(""), None);
    }
}
