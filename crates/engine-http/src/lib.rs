//! `HttpEngine` portunun reqwest implementasyonu.
//!
//! `layer-core`'u tanımaz. Sözleşme `layer-engine-api`.
//!
//! Akış: probe → (range_ok && toplam biliniyor && max_segments > 1) ise segmented
//! (Task 9), değilse tek bağlantı. Probe 5xx retry edilir. İndirme gövdesinde
//! 403/410 → `NeedReResolve` (kör retry yok). Probe 403 range reddi sayılır.

mod download;
mod http;
mod probe;

use download::{run_single, SingleOutcome};
use http::{policy_for_tests, RetryPolicy};
use layer_engine_api::{
    ByteBudget, EngineError, EngineEvent, FailureReason, HttpEngine, HttpTaskSpec,
};
use probe::ProbeFail;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct ReqwestHttpEngine {
    client: reqwest::Client,
    retry: RetryPolicy,
}

impl ReqwestHttpEngine {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            client,
            retry: RetryPolicy::default(),
        }
    }

    /// Testler: kısa backoff, az deneme.
    pub fn for_tests() -> Self {
        Self {
            retry: policy_for_tests(),
            ..Self::new()
        }
    }
}

impl Default for ReqwestHttpEngine {
    fn default() -> Self {
        Self::new()
    }
}

fn budgets_for(spec: &HttpTaskSpec, global: Option<Arc<ByteBudget>>) -> Vec<Arc<ByteBudget>> {
    let mut out = Vec::new();
    if let Some(bps) = spec.speed_limit_bps.filter(|n| *n > 0) {
        out.push(Arc::new(ByteBudget::new(bps)));
    }
    if let Some(g) = global {
        out.push(g);
    }
    out
}

#[async_trait::async_trait]
impl HttpEngine for ReqwestHttpEngine {
    async fn run(
        &self,
        spec: HttpTaskSpec,
        events: mpsc::UnboundedSender<EngineEvent>,
        cancel: CancellationToken,
        global_limit: Option<Arc<ByteBudget>>,
    ) -> Result<(), EngineError> {
        if !(spec.url.starts_with("http://") || spec.url.starts_with("https://")) {
            return Err(EngineError::InvalidSpec(format!(
                "yalnız http(s): {}",
                spec.url
            )));
        }
        if spec.file_name.is_empty() || spec.file_name.contains(['/', '\\']) {
            return Err(EngineError::InvalidSpec(
                "file_name boş veya yol içeriyor".into(),
            ));
        }

        let probe = match self.probe_with_retry(&spec, &cancel).await {
            Ok(p) => p,
            Err(ProbeStop::Cancelled) => return Ok(()),
            Err(ProbeStop::Failed(reason)) => {
                let _ = events.send(EngineEvent::Failed(reason));
                return Ok(());
            }
        };
        if cancel.is_cancelled() {
            return Ok(());
        }
        let _ = events.send(EngineEvent::Probed {
            total_bytes: probe.total_bytes,
            etag: probe.etag.clone(),
            last_modified: probe.last_modified.clone(),
            range_ok: probe.range_ok,
        });

        // Task 9 segmented'i buraya bağlar. M1 Task 8: her zaman tek bağlantı.
        let budgets = budgets_for(&spec, global_limit);
        match run_single(
            &self.client,
            &spec,
            &probe,
            &events,
            &cancel,
            &budgets,
            &self.retry,
        )
        .await?
        {
            SingleOutcome::Completed => {
                let _ = events.send(EngineEvent::Done);
            }
            SingleOutcome::Cancelled => {}
            SingleOutcome::Conflict => {
                let _ = events.send(EngineEvent::ConflictDetected);
            }
            SingleOutcome::NeedReResolve => {
                let _ = events.send(EngineEvent::NeedReResolve);
            }
            SingleOutcome::Failed(reason) => {
                let _ = events.send(EngineEvent::Failed(reason));
            }
        }
        Ok(())
    }
}

enum ProbeStop {
    Cancelled,
    Failed(FailureReason),
}

impl ReqwestHttpEngine {
    async fn probe_with_retry(
        &self,
        spec: &HttpTaskSpec,
        cancel: &CancellationToken,
    ) -> Result<probe::ProbeResult, ProbeStop> {
        let mut attempt = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(ProbeStop::Cancelled);
            }
            match probe::probe(&self.client, spec).await {
                Ok(p) => return Ok(p),
                Err(ProbeFail::Transient { status, message }) => {
                    attempt += 1;
                    if attempt >= self.retry.max_attempts {
                        return Err(probe_failed(status, message));
                    }
                    if !http::sleep_backoff(attempt, &self.retry, cancel).await {
                        return Err(ProbeStop::Cancelled);
                    }
                }
                Err(ProbeFail::Http { status, message }) => {
                    return Err(probe_failed(Some(status), message));
                }
                Err(ProbeFail::Io(m)) => {
                    return Err(ProbeStop::Failed(FailureReason::Io(m)));
                }
            }
        }
    }
}

fn probe_failed(status: Option<u16>, message: String) -> ProbeStop {
    ProbeStop::Failed(match status {
        Some(status) => FailureReason::Http { status, message },
        None => FailureReason::Network(message),
    })
}
