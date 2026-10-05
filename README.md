# layer

kendi indirme yöneticin: segmented HTTP(S) + torrent + "link yapıştır" medya çözme. Tauri masaüstü uygulaması, Windows öncelikli.

## Durum

- Segmented HTTP, torrent (aria2) ve sayfa linki (yt-dlp + ffmpeg)
- Hız tavanı: görev başına ve tüm uygulama (MB/s)
- Torrent dosya seçimi, kapanınca kaldığı yerden video/torrent devamı
- Bitti bildirimi, Sharp/Soft tema, küp ilerleme
- Sidecar'lar (`yt-dlp`, `ffmpeg`, `aria2c`) repoda yok: `sidecars/bin`

## Kurulum

```
cd apps/desktop
pnpm install
pnpm tauri build
```

Kurulum dosyası: `apps/desktop/src-tauri/target/release/bundle/nsis/`. Konsolsuz çalıştırılabilir: `apps/desktop/src-tauri/target/release/layer-desktop.exe`.

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
