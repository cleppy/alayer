use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// Zaman tabanlı token-bucket byte bütçesi.
///
/// İki seviyede kullanılır: görev-bazlı (`HttpTaskSpec::speed_limit_bps`) ve
/// global (core'un kurduğu `Arc<ByteBudget>`). Segmentler/parçalar `acquire(n)`
/// ile byte hakkı ister; bütçe boşsa refill hızında uyutulur.
#[derive(Debug)]
pub struct ByteBudget {
    capacity_bps: u64,
    /// Birikmiş kredi (byte). Başlangıçta capacity kadar (burst izni).
    tokens: Mutex<f64>,
    /// Kredi talepleri sıraya girer (fairness): aynı anda tek bekleyen grubu.
    gate: Mutex<()>,
    last_refill_ms: AtomicU64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl ByteBudget {
    pub fn new(capacity_bps: u64) -> Self {
        Self {
            capacity_bps,
            tokens: Mutex::new(capacity_bps as f64),
            gate: Mutex::new(()),
            last_refill_ms: AtomicU64::new(now_ms()),
        }
    }

    /// `n` byte hakkı ister; kredi yetmiyorsa refill hızına kadar uyur.
    /// `cancel` tetiklenirse hakkı beklemeden (kısmi) döner — çağıran taraf
    /// cancel sonrası yazmayı bırakır.
    pub async fn acquire(&self, n: u64, cancel: &CancellationToken) {
        if self.capacity_bps == 0 || n == 0 {
            return;
        }
        let _queued = self.gate.lock().await;
        let mut remaining = n as f64;
        while remaining > 0.0 {
            if cancel.is_cancelled() {
                return;
            }
            // Refill: son bakıştan beri geçen süre * kapasite.
            let mut tokens = self.tokens.lock().await;
            let now = now_ms();
            let last = self.last_refill_ms.swap(now, Ordering::SeqCst);
            let elapsed_s = now.saturating_sub(last) as f64 / 1000.0;
            *tokens = (*tokens + elapsed_s * self.capacity_bps as f64)
                .min(self.capacity_bps as f64 * 2.0); // burst tavanı: 2 saniyelik kredi
            let take = remaining.min(*tokens);
            *tokens -= take;
            remaining -= take;
            if remaining <= 0.0 {
                break;
            }
            // Ne kadar uyumalı: kalan byte / hız.
            let sleep_ms = (remaining / self.capacity_bps as f64 * 1000.0).ceil() as u64;
            drop(tokens);
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_millis(sleep_ms.clamp(1, 2000))) => {}
                _ = cancel.cancelled() => return,
            }
        }
    }

    /// Test/ayar için: saniyede byte kapasitesi.
    pub fn capacity_bps(&self) -> u64 {
        self.capacity_bps
    }
}

/// Birden çok bütçeyi (görev + global) tek `acquire`'da birleştirir.
pub async fn acquire_all(budgets: &[Arc<ByteBudget>], n: u64, cancel: &CancellationToken) {
    for b in budgets {
        b.acquire(n, cancel).await;
        if cancel.is_cancelled() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn budget_paces_bytes() {
        let b = ByteBudget::new(10_000); // 10 KB/s
        let cancel = CancellationToken::new();
        let start = tokio::time::Instant::now();
        b.acquire(10_000, &cancel).await; // mevcut krediyle anında
        b.acquire(5_000, &cancel).await; // ~0.5s beklemeli
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(350),
            "çok hızlı: {elapsed:?}"
        );
        assert!(
            elapsed < Duration::from_millis(1500),
            "gereksiz yavaş: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn budget_zero_capacity_is_free() {
        let b = ByteBudget::new(0);
        let cancel = CancellationToken::new();
        let start = tokio::time::Instant::now();
        b.acquire(1_000_000, &cancel).await;
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn budget_acquire_cancellable() {
        let b = ByteBudget::new(1);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let start = tokio::time::Instant::now();
        b.acquire(1_000_000, &cancel).await; // hemen dönmeli
        assert!(start.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn budget_mid_wait_cancel() {
        let b = ByteBudget::new(100); // çok yavaş
        let cancel = CancellationToken::new();
        let c2 = cancel.clone();
        let h = tokio::spawn(async move {
            b.acquire(10_000, &c2).await;
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
        // join hemen dönmeli
        tokio::time::timeout(Duration::from_millis(500), h)
            .await
            .expect("acquire cancel ile zamanında dönmedi")
            .unwrap();
    }

    #[tokio::test]
    async fn acquire_all_waits_for_slowest() {
        let fast = Arc::new(ByteBudget::new(1_000_000));
        let slow = Arc::new(ByteBudget::new(2_000));
        let cancel = CancellationToken::new();
        let start = tokio::time::Instant::now();
        // slow: 2000 kredi var, 4000 isteyince ~1s refill bekler
        acquire_all(&[fast, slow], 4_000, &cancel).await;
        assert!(start.elapsed() >= Duration::from_millis(500));
    }
}
