# Execution log — M0+M1 plan

Plan: `docs/superpowers/plans/2026-10-05-layer-m0-m1-core-http.md`

- [x] Task 1: Workspace iskeleti (M0)
- [x] Task 2: engine-api — DTO'lar
- [x] Task 3: engine-api — portlar + ByteBudget
- [x] Task 4: core — Task entity + durum makinesi
- [x] Task 5: core — SQLite repository
- [x] Task 6: core — kuyruk politikası
- [x] Task 7: core — TaskManager + TaskIntake
- [x] Task 8: engine-http — probe + tek bağlantı
- [x] Task 9: engine-http — segmented + resume + conflict
- [x] Task 10: engine-http — retry/backoff
- [x] Task 11: layer-cli — composition root + smoke

Doğrulama: `cargo test --workspace` yeşil (core 37, engine-api 12, engine-http 21). `cargo clippy --workspace --all-targets -- -D warnings` temiz. Smoke: `layer-cli add http://127.0.0.1:8765/f.bin` → 200000 byte, SHA256 eşleşti, exit 0.

Sapmalar:
- Probe 403 `NeedReResolve` değil (CDN Range reddi). `NeedReResolve` yalnız indirme gövdesinde 403/410.
- Global hız limiti `HttpEngine::run` argümanı (`TaskManager::set_global_limit`).
- ETag uyuşmazlığı probe sonrası, indirmeden önce `Conflict`.
