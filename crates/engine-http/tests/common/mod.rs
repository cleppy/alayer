//! Range-aware test sunucusu. wiremock gövdeyi isteğe göre dilimleyemez.

use std::net::SocketAddr;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

pub struct RangeServer {
    pub addr: SocketAddr,
}

pub async fn spawn_range_server(body: Vec<u8>, etag: &str) -> RangeServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let etag = etag.to_string();
    let payload = body.clone();
    let etag_c = etag.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = listener.accept().await else { break };
            let payload = payload.clone();
            let etag = etag_c.clone();
            tokio::spawn(async move { serve_one(sock, &payload, &etag).await });
        }
    });
    RangeServer { addr }
}

async fn serve_one(sock: tokio::net::TcpStream, body: &[u8], etag: &str) {
    let (r, mut w) = tokio::io::split(sock);
    let mut reader = BufReader::new(r);
    let mut range: Option<(u64, Option<u64>)> = None;
    let mut if_range: Option<String> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("range:") {
            range = parse_range(v.trim());
        }
        if let Some(v) = line.strip_prefix("If-Range:").or_else(|| line.strip_prefix("if-range:")) {
            if_range = Some(v.trim().trim_end_matches('\r').to_string());
        }
    }
    // İstemci gövde beklemiyor olabilir; kalanı yutma, yanıt yaz.
    let _ = reader;

    // If-Range uyuşmazsa tam gövde 200 (kaynak değişti).
    let range_honored = match (&range, &if_range) {
        (Some(_), Some(ir)) if ir != etag => false,
        (Some(_), _) => true,
        _ => false,
    };

    if range_honored {
        let (start, end) = range.unwrap();
        let end = end.unwrap_or(body.len() as u64 - 1).min(body.len() as u64 - 1);
        let start = start.min(end);
        let slice = &body[start as usize..=end as usize];
        let head = format!(
            "HTTP/1.1 206 Partial Content\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nContent-Range: bytes {start}-{end}/{}\r\nETag: {etag}\r\nConnection: close\r\n\r\n",
            slice.len(),
            body.len()
        );
        if w.write_all(head.as_bytes()).await.is_ok() {
            let _ = w.write_all(slice).await;
        }
    } else {
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nETag: {etag}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n\r\n",
            body.len()
        );
        if w.write_all(head.as_bytes()).await.is_ok() {
            let _ = w.write_all(body).await;
        }
    }
    let _ = w.flush().await;
    // bağlantıyı kapat
    drop(w);
}

fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let v = v.strip_prefix("bytes=")?;
    let (a, b) = v.split_once('-')?;
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() { None } else { b.parse().ok() };
    Some((start, end))
}
