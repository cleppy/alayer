# layer

kendi indirme yöneticin: segmented HTTP(S) + torrent + "link yapıştır" medya çözme. Tauri masaüstü uygulaması, Windows öncelikli.

## Durum

- **M1 ✅** — headless çekirdek: `engine-api`, `core` (SQLite + kuyruk + TaskManager), `engine-http` (segmented, resume, If-Range conflict, retry), `layer-cli`
- **M2 (kabuk ayakta)** — Tauri + Svelte kuyruk: link yapıştır, duraklat, sürdür, iptal. Ayarlar paneli henüz yok.
- M3 — torrent köprüsü (aria2 sidecar)
- M4 — medya çözücü (yt-dlp sidecar, ffmpeg mux-only)
- M5 — clipboard listener, kategoriler, paketleme

## Repo düzeni

```
crates/engine-api    # portlar (trait'ler) + DTO'lar — iç halka sözleşmesi
crates/core          # domain + use-case'ler; sadece engine-api'yi tanır; SQLite tek doğruluk kaynağı
crates/engine-http   # HttpEngine impl (reqwest); core'u bilmez
apps/cli             # headless composition root (smoke)
apps/desktop         # (M2) Tauri kabuk + Svelte UI
docs/                # spec + planlar
```

Mimari ve hata senaryoları: [`docs/superpowers/specs/2026-10-05-layer-design.md`](docs/superpowers/specs/2026-10-05-layer-design.md)

## Geliştirme

```
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo run -p layer-cli -- add <url> --dir <klasör>
cd apps/desktop
pnpm tauri dev
```
