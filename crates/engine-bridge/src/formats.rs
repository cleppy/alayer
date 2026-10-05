//! yt-dlp format listesini az ve anlaşılır seçeneğe indirir.
//! 40 codec satırı değil: görüntü basamakları ve ses.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatChoice {
    pub id: String,
    /// Düğmenin üstündeki söz: `1080` veya `mp3`.
    pub label: String,
    /// `video` veya `audio`.
    pub group: String,
    pub format: String,
    pub audio_format: Option<String>,
}

pub fn curate(info: &Value) -> Vec<FormatChoice> {
    let formats = info["formats"].as_array();
    let max_height = formats
        .map(|list| {
            list.iter()
                .filter(|f| is_video(f))
                .filter_map(height)
                .max()
                .unwrap_or(0)
        })
        .unwrap_or(0);
    let mut out = Vec::new();
    for h in [2160, 1440, 1080, 720, 480, 360, 240] {
        if max_height >= h {
            out.push(FormatChoice {
                id: format!("v{h}"),
                label: h.to_string(),
                group: "video".into(),
                format: format!(
                    "bv*[height<={h}][ext=mp4]+ba[ext=m4a]/bv*[height<={h}]+ba/b[height<={h}]/b"
                ),
                audio_format: None,
            });
        }
    }
    if out.is_empty() && formats.is_some_and(|list| list.iter().any(is_video)) {
        out.push(FormatChoice {
            id: "vbest".into(),
            label: "en iyi".into(),
            group: "video".into(),
            format: "bv*+ba/b".into(),
            audio_format: None,
        });
    }
    let has_audio = formats.is_some_and(|list| list.iter().any(is_audio));
    if has_audio {
        out.push(FormatChoice {
            id: "mp3".into(),
            label: "mp3".into(),
            group: "audio".into(),
            format: "ba/b".into(),
            audio_format: Some("mp3".into()),
        });
        out.push(FormatChoice {
            id: "m4a".into(),
            label: "m4a".into(),
            group: "audio".into(),
            format: "ba[ext=m4a]/ba".into(),
            audio_format: None,
        });
    }
    out
}

fn height(format: &Value) -> Option<u64> {
    format["height"].as_u64().filter(|h| *h > 0)
}

fn is_video(format: &Value) -> bool {
    match format["vcodec"].as_str() {
        Some("none") | None => false,
        Some(_) => true,
    }
}

fn is_audio(format: &Value) -> bool {
    match format["acodec"].as_str() {
        Some("none") | None => false,
        Some(_) => true,
    }
}

pub fn title_of(info: &Value) -> String {
    info["title"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or("video")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        serde_json::json!({
            "title": "Demo",
            "formats": [
                {"vcodec": "avc1", "acodec": "none", "height": 1080, "ext": "mp4"},
                {"vcodec": "avc1", "acodec": "none", "height": 360, "ext": "mp4"},
                {"vcodec": "none", "acodec": "mp4a", "ext": "m4a"}
            ]
        })
    }

    #[test]
    fn offers_heights_at_or_below_source_plus_audio() {
        let choices = curate(&sample());
        let labels: Vec<_> = choices.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"1080"));
        assert!(labels.contains(&"720"));
        assert!(labels.contains(&"360"));
        assert!(!labels.contains(&"2160"), "kaynak 1080, 4K yok");
        assert!(labels.contains(&"mp3"));
        assert!(labels.contains(&"m4a"));
        let p1080 = choices.iter().find(|c| c.label == "1080").unwrap();
        assert_eq!(p1080.group, "video");
        assert!(p1080.audio_format.is_none());
        let mp3 = choices.iter().find(|c| c.id == "mp3").unwrap();
        assert_eq!(mp3.audio_format.as_deref(), Some("mp3"));
    }
}
