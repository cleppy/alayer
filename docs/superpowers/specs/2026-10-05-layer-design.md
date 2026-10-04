# layer — Tasarım Spec'i (v1)

Tarih: 2026-10-05
Durum: Onaylandı (kullanıcı incelemesi atlandı, doğrudan uygulamaya geçildi)

## 1. Amaç ve Kapsam

**layer**, Windows öncelikli (Tauri sayesinde taşınabilir) bir masaüstü indirme yöneticisidir:

- **Çekirdek IDM:** çok segmentli HTTP(S) indirme, kaldığı yerden devam (resume), kuyruk, hız sınırı, kategori/klasör kuralları.
- **Torrent motoru:** magnet + `.torrent`, DHT/PEX, dosya seçmeli indirme — **aria2 sidecar** üzerinden (libtorrent FFI riskinden kaçınıldı: A+B hibrit yaklaşım).
- **Medya çözücü (cobalt tarzı, lokal):** "Link yapıştır" → `yt-dlp` sidecar ile çözüm (`--dump-json`), format seçimi, indirme.
- **Pano izleyici:** kopyalanan URL/magnet yakalanır; varsayılan davranış **sor**, sessiz ekleme ayar arkası.

**Kapsam dışı (sonraki fazlar):** web servisi / seedbox / cobalt tarzı hosted link downloader, landing page (dağıtım GitHub Releases), tarayıcı eklentisi (core'da `TaskIntake` kapısı hazır durur), video sniffing, scheduler/zamanlayıcı.

## 2. Mimari — Crate Grafiği ve Dependency Rule

Bağımlılık okları her zaman içe (domain'e) bakar. Tauri, aria2, yt-dlp, reqwest dış halka detaylarıdır.

```
crates/engine-api      # portlar (trait'ler) + paylaşılan DTO'lar
crates/core            # domain + use-case'ler; SADECE engine-api'yi tanır
crates/engine-http     # HttpEngine impl (reqwest); engine-api'yi bilir, core'u BİLMEZ
crates/engine-bridge   # TorrentEngine (aria2 JSON-RPC) + MediaResolver (yt-dlp) impl'leri
apps/desktop/src-tauri # composition root + ince kabuk (komut/event/sidecar spawn)
apps/desktop/ui        # Svelte + TypeScript + Vite
```

### engine-api (portlar + DTO'lar)

- `HttpEngine`, `TorrentEngine`, `MediaResolver`, `ClipboardWatcher`, `EventSink` trait'leri
- `TaskId`, `TaskKind { Http | Torrent | Media }`, `TaskState`, `Progress`, `EngineEvent`, `EngineError`
- `HttpTaskSpec { url, headers, dest_dir, file_name, segments, speed_limit_bps, resume_state }`
- `ResolverContext { original_url, extractor, format_id, expected_size }`

### core (domain)

- `Task` durum makinesi: `Queued → Probing → Downloading ⇄ Paused → Completed | Failed(reason) | Conflict`
- Kuyruk politikası: eşzamanlı görev limiti, FIFO + öncelik
- Hız limiti bütçesi (global), kategori/klasör kuralları
- **SQLite = tek doğruluk kaynağı** (rusqlite, core içinde): görevler, segment planları, ETag/Last-Modified, aria2 GID eşlemesi (GID sadece cache, kimlik değil), `ResolverContext`, ayarlar, geçmiş
- `TaskIntake` kapısı: dışarıdan görev ekleme (`source: Ui | Clipboard | Cli | Extension`); kaynak bilgisi core'da davranış değiştirmez, sadece loglanır
- `TaskManager`: use-case orkestrasyonu — engine'leri trait üzerinden çağırır, `EventSink` ile dışarı olay yayar

### src-tauri (composition root + kabuk)

- Tauri komutları use-case'lere delege eder; domain mantığı içermez
- `EventSink` adapter'ı → Tauri event (throttle ~4Hz burada)
- Sidecar spawn + sağlık kontrolü (SHA-256 pin doğrulama, `--version` probe)
- `tauri-plugin-single-instance`: ikinci açılış mevcut pencereyi odaklar, argv/deep-link URL'leri `TaskIntake`'e düşer
- `ClipboardWatcher` Windows implementasyonu (en dış halka = OS detayı)

### UI (Svelte + TS)

Svelte seçiminin gerekçesi: form-ağır arayüz, küçük paket boyutu, Tauri ile düşük sürtünme. UI yalnızca komut/event bilir.

## 3. Bileşen Detayları

### 3.1 engine-http (kendi motorumuz)

- **Probe:** görev başında ilk `Range: bytes=0-0` isteği → `206` mı, `Content-Range` toplamı, `ETag`/`Last-Modified`, `Accept-Ranges`.
- **Host-bazlı bağlantı politikası:** probe 206 vermiyorsa / `Content-Length` yoksa / `Accept-Ranges` yalan söylüyorsa (başlık evet diyor ama 200 dönüyor) → **tek bağlantı** + görev `unresumable` işaretlenir. Varsayılan segment sayısı global ayardır (8); host override tablosu DB'de.
- **Segment indirme:** tek `.part` dosyası, segmentler `seek+write` ile kendi offset'lerine yazar (birleştirme adımı yok). Bitince final ada rename.
- **Resume doğrulaması:** her segment isteğinde `If-Range: <ETag|Last-Modified>`; sunucu `200` dönerse kaynak değişmiştir → parçalar birleştirilmez, görev **`Conflict`**.
- **Retry:** transient ağ hataları → segment bazlı backoff'lu retry. `403/410` → kör retry YOK; `NeedReResolve` eventi (aşağıda).
- **Hız limiti:** paylaşılan byte-budget token bucket (görev + global).

### 3.2 Ölen URL akışı (googlevideo vb.)

- Görevde `ResolverContext` saklanır. `403/410` → **aynı `format_id`** için tek yeniden çözüm (`MediaResolver::refresh`).
- Yeni URL + `http_headers` seti (User-Agent, Referer, Cookie — `--dump-json`'dan aynen) engine-http'e verilir.
- Beklenen boyut ve format aynıysa → segmentler kaldığı yerden yeni URL'le devam.
- Format/boyut değiştiyse → görev durur, kullanıcıya "baştan indirilsin mi" sorulur.
- Yeniden çözüm de 403 verirse → `Failed(resolver)`, stderr'den sınıflandırılmış sebep UI'da.

### 3.3 engine-bridge / torrent (aria2)

- Sidecar: `--enable-rpc --rpc-listen-all=false --rpc-secret=<token> --rpc-listen-port=<rust'un seçtiği rastgele boş port> --save-session=<path> --save-session-interval=30 --bt-save-metadata=true`
- RPC yalnız `127.0.0.1`.
- **Ekleme her zaman duraklatılmış** (`pause=true`):
  - `.torrent`: dosya listesi Rust'ta **lokal bencode parse** ile alınır → kullanıcı seçer → `select-file` ile eklenir → başlatılır.
  - magnet: metadata gelince `bt-save-metadata` ile `.torrent` diske düşer → GID silinir → `select-file` ile `.torrent` olarak yeniden eklenir → başlatılır.
- İndirme klasörü her zaman görev kaydındaki klasör (DB ile tutarlı).
- **Kurtarma (session kaybı):** graceful kapanışta `--save-session`, periyodik `--save-session-interval`; açılışta `--input-file`. Sert öldürmede **DB-driven reconciliation**: SQLite'taki torrent görevleri `tellActive/tellWaiting/tellStopped` ile karşılaştırılır; GID bilinmiyorsa görev duraklatılmış yeniden eklenir; aria2 indirme dizinindeki `.aria2` control dosyasından piece'lere devam eder; yeni GID DB'ye yazılır. Re-add sonrası infohash/boyut uyuşmazsa → `Conflict`, dosyalara dokunulmaz.

### 3.4 engine-bridge / medya çözücü (yt-dlp)

- Çözüm: `yt-dlp --dump-json <url>` → format listesi UI'da (kalite / sadece ses).
- **Tek dosya** (progressive) → final URL + `http_headers` → **engine-http** indirir.
- **Video+ses ayrı** (DASH vb.) → yt-dlp kendisi indirir + **ffmpeg yalnız mux** için (transcode yok). Sonuç dosya tamamlanmış görev olarak kaydedilir.
- Her çalıştırma sonrası doğrulama: exit code **ve** dosya var **ve** boyut > 0 **ve** `filesize_approx` biliniyorsa tolerans içinde. Exit 0 + sıfır byte = başarı değil → bir kez taze çözümle retry → yine sıfırsa `Failed` + sınıflandırılmış stderr mesajı (`geo-blocked`, `login-required`, `unavailable`, `unknown`).
- Merge sonrası da doğrulama: final boyut ≈ video+ses toplamı.

### 3.5 Pano izleyici

- Yoklama yok: Windows'ta message-only pencere + `AddClipboardFormatListener` → `WM_CLIPBOARDUPDATE` (yedek: `GetClipboardSequenceNumber` değişim kapısı). Değişmemiş pano okunmaz.
- Yakalanan URL/magnet → ayara göre `TaskIntake(source=Clipboard)`: varsayılan **sor** (onay diyaloğu), sessiz ekleme ayar arkası.

### 3.6 Sidecar bütünlüğü ve antivirüs

- Açılışta sağlık kontrolü: her sidecar için `--version` spawn + **SHA-256 pin doğrulaması** (pinler repoda; güncelleme aracı pini yeniler).
- Spawn başarısız/binary yok → **degrade mode**: HTTP çekirdeği süreç-içi Rust olduğundan etkilenmez; torrent + medya çözme UI'da "onarım gerekli" durumuna düşer.
- Onarım akışı: resmi kaynaktan (aria2 GitHub release, yt-dlp release, ffmpeg build) yeniden indir → checksum doğrula → gerekirse AV exclusion talimatı.
- Paketleme: NSIS installer, sidecar'lar resmi upstream binary'leri. Kod imza sertifikası = opsiyonel (bütçe işi; SmartScreen notu).

## 4. Test Stratejisi

- **core:** %100 headless unit test — port'lardan fake engine'ler (script'li progress/hata), temp SQLite. Durum makinesi geçişleri, kuyruk politikası, If-Range conflict kararı, reconciliation, TaskIntake. UI'sız, sidecar'sız koşar.
- **engine-http:** lokal test HTTP sunucusu (wiremock): tam Range desteği, Range'siz, yalancı Accept-Ranges, ETag değişimi, indirme ortasında 403, Content-Length'siz chunked, hız sınırı.
- **engine-bridge:** gerçek `aria2c` sidecar'ına karşı entegrasyon (CI'a pinlenmiş binary): duraklatılmış ekleme, select-file, **proses kill → reconciliation kurtarma testi**. yt-dlp: canned JSON fixture'lar + sahte yt-dlp executable (sıfır byte, 403, exit-code); canlı smoke testler opsiyonel, CI'da default kapalı.
- **UI/kabuk:** komutlar ince olduğundan asıl test core'da; vitest ile store/component testleri M2'den sonra. Single-instance ve clipboard = Windows manuel doğrulama checklist'i.

## 5. Milestone'lar

- **M0** — repo iskeleti, Cargo workspace, sidecar pin+checksum aracı, Windows CI build
- **M1** — core + engine-http headless: segmented indirme, resume, If-Range conflict, tek-bağlantı fallback, kuyruk, hız limiti, SQLite
- **M2** — Tauri kabuk + Svelte UI: görev listesi, ekleme diyalogları, event akışı, ayarlar, single-instance, TaskIntake
- **M3** — torrent köprüsü: aria2 lifecycle, duraklatılmış ekleme akışı, dosya seçimi, reconciliation/kurtarma
- **M4** — medya çözücü: yt-dlp akışı, format seçici, header devri, 403'te yeniden çözüm, mux-only ffmpeg, sıfır-byte doğrulama
- **M5** — clipboard listener, kategori/klasör kuralları, paketleme, GitHub Release

Her milestone kendi plan → uygulama → test döngüsünü alır. Planlar: `docs/superpowers/plans/`.

## 6. Global Kısıtlar

- Rust stable (MSVC toolchain), edition 2021+
- UI: Svelte 5 + TypeScript + Vite
- DB: SQLite (rusqlite), tek dosya, app data dizininde
- Sidecar'lar: aria2c, yt-dlp, ffmpeg — resmi upstream binary'leri, SHA-256 pin'li
- ffmpeg yalnız mux için kullanılır
- GID kalıcı kimlik değildir; task id kalıcı kimliktir
- v1 hedef platform: Windows (x64); macOS/Linux sonraki faz
