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
        Some(TaskKind::Http)
    } else {
        None
    }
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
        assert_eq!(classify_url("ftp://e.com/f"), None);
        assert_eq!(classify_url("file:///C:/x"), None);
        assert_eq!(classify_url(""), None);
    }
}
