# layer M2: Tauri kabuk + Svelte kuyruk

**Goal:** Windows masaüstü kabuğu. Kullanıcı link yapıştırır, kuyruğu görür, duraklatır, sürdürür, iptal eder. Domain core'da kalır.

**Architecture:** `apps/desktop/src-tauri` composition root. Komutlar `TaskManager`'a delege eder. `EventSink` adaptörü `CoreEvent`'i UI DTO'suna çevirir, progress'i ~4Hz kısar. UI yalnız `invoke` / `listen` bilir.

**Tech:** Tauri 2, Svelte 5, Vite, TypeScript. İndirme klasörü: `Downloads/layer`. DB: app data `layer.db`.

## Görünüm

İndirme masası, film şeridi. Varsayılan koyu-asidik veya krem-serif değil.

- Bench `#1c2430`, unexposed `#2a3444`, exposure `#f3f6fb`, leader `#e6b325`, fail `#e15b52`, ink `#e8edf4`
- Syne yalnız sözcük işareti, Source Sans 3 arayüz, IBM Plex Mono sayaç
- İmza: satırın kendisi kare. Sol şerit dişi delik. Dolu kısım pozlanmış (koyu yazı), gerisi ham (açık yazı)

## Görevler

1. Kabuk: `list_tasks`, `add_url`, `pause`, `resume`, `cancel`. Event `layer`.
2. Svelte kuyruk + yapıştır çubuğu.
3. `tauri-plugin-single-instance`: ikinci açılış ana pencereyi öne alır.
4. `pnpm build` ve `cargo check -p layer-desktop` yeşil.
