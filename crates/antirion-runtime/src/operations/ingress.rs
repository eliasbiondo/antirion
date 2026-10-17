//! Ingress pauses: an incident brake that refuses new data-plane requests, gateway-wide or for one
//! organization, until it is resumed or its duration ends. Pauses are rows of the shared store, so
//! every node sees them; each node keeps the active ones in memory for the request path and reads
//! the store again at least every [`REFRESH`].

use std::sync::Arc;
use std::time::Duration;

use antirion_core::UtcMicros;
use antirion_store::{PendingRequest, RequestFilter, StoreError};
use arc_swap::ArcSwap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Runtime;

/// Reasons of the ingress rules.
pub mod reasons {
    antirion_error::reasons! {
        INGRESS_PAUSED = "ingress_paused" => Overloaded,
    }
}

/// The store kind of a pause row; its id is [`Pause::id`].
pub const KIND: &str = "ingress_pause";
/// How often a node reads the pauses other nodes wrote.
pub const REFRESH: Duration = Duration::from_millis(500);
/// The longest pause.
pub const MAX_DURATION: Duration = Duration::from_secs(24 * 3600);

const ACTIVE: &str = "active";
const ENDED: &str = "ended";
/// Attempts of a write that lost a compare-and-set to another writer.
const ATTEMPTS: usize = 3;

/// One pause.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pause {
    /// The paused organization; `None` pauses the whole gateway.
    pub organization: Option<String>,
    pub reason: String,
    pub paused_by: String,
    pub paused_at_us: i64,
    pub resumes_at_us: i64,
}

impl Pause {
    /// The row id of a pause of `organization` (or of the gateway).
    #[must_use]
    pub fn id_of(organization: Option<&str>) -> String {
        match organization {
            Some(o) => format!("organization/{o}"),
            None => "gateway".to_owned(),
        }
    }

    #[must_use]
    pub fn id(&self) -> String {
        Pause::id_of(self.organization.as_deref())
    }

    /// Whether the pause still holds at `now`.
    #[must_use]
    pub fn holds(&self, now: UtcMicros) -> bool {
        now.0 < self.resumes_at_us
    }

    /// The Admin API's representation.
    #[must_use]
    pub fn shown(&self) -> Value {
        json!({
            "organization": self.organization,
            "reason": self.reason,
            "paused_by": self.paused_by,
            "paused_at": UtcMicros(self.paused_at_us).to_rfc3339(),
            "resumes_at": UtcMicros(self.resumes_at_us).to_rfc3339(),
        })
    }
}

/// The pauses a node enforces.
#[derive(Debug, Default)]
pub struct Pauses {
    current: ArcSwap<Vec<Pause>>,
}

impl Pauses {
    /// The pauses that hold at `now`, the gateway-wide one first.
    #[must_use]
    pub fn active(&self, now: UtcMicros) -> Vec<Pause> {
        let mut out: Vec<Pause> = self
            .current
            .load()
            .iter()
            .filter(|p| p.holds(now))
            .cloned()
            .collect();
        out.sort_by(|a, b| a.organization.cmp(&b.organization));
        out
    }

    /// How long a new request is refused for: the gateway-wide pause when `organization` is
    /// `None`, else that organization's.
    #[must_use]
    pub fn refusal(&self, now: UtcMicros, organization: Option<&str>) -> Option<Duration> {
        let pauses = self.current.load();
        if pauses.is_empty() {
            return None;
        }
        pauses
            .iter()
            .find(|p| p.organization.as_deref() == organization && p.holds(now))
            .map(|p| UtcMicros(p.resumes_at_us).since(now))
    }

    fn set(&self, pauses: Vec<Pause>) {
        self.current.store(Arc::new(pauses));
    }

    /// Forgets every pause (test mode).
    pub fn clear(&self) {
        self.set(Vec::new());
    }
}

/// What a resume found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resumed {
    /// The scope was paused and is not any more.
    Ended,
    /// Nothing held for the scope.
    NotPaused,
}

impl Runtime {
    /// Pauses ingress as `pause` says, replacing a pause of the same scope.
    pub async fn pause_ingress(&self, pause: &Pause) -> Result<(), StoreError> {
        let id = pause.id();
        let body = serde_json::to_string(pause)?;
        for _ in 0..ATTEMPTS {
            let written = match self.store.request(KIND, &id).await? {
                Some(row) => {
                    self.store
                        .update_request(KIND, &id, row.version, ACTIVE, &body)
                        .await?
                }
                None => {
                    self.store
                        .insert_request(PendingRequest {
                            kind: KIND.to_owned(),
                            id: id.clone(),
                            subject: pause.organization.clone().unwrap_or_default(),
                            state: ACTIVE.to_owned(),
                            author: pause.paused_by.clone(),
                            created_us: pause.paused_at_us,
                            version: 0,
                            body: body.clone(),
                        })
                        .await?
                }
            };
            if written {
                self.refresh_pauses().await;
                return Ok(());
            }
        }
        Err(StoreError::Conflict)
    }

    /// Ends the pause of `organization` (or the gateway-wide one).
    pub async fn resume_ingress(&self, organization: Option<&str>) -> Result<Resumed, StoreError> {
        let id = Pause::id_of(organization);
        let now = self.now();
        for _ in 0..ATTEMPTS {
            let Some(row) = self.store.request(KIND, &id).await? else {
                return Ok(Resumed::NotPaused);
            };
            let held =
                row.state == ACTIVE && serde_json::from_str::<Pause>(&row.body).is_ok_and(|p| p.holds(now));
            if !held {
                return Ok(Resumed::NotPaused);
            }
            if self
                .store
                .update_request(KIND, &id, row.version, ENDED, &row.body)
                .await?
            {
                self.refresh_pauses().await;
                return Ok(Resumed::Ended);
            }
        }
        Err(StoreError::Conflict)
    }

    /// Reads the pauses from the store; on failure the node keeps enforcing what it knew.
    pub async fn refresh_pauses(&self) {
        let rows = self
            .store
            .requests(
                KIND,
                &RequestFilter {
                    states: vec![ACTIVE.to_owned()],
                    limit: 10_000,
                    ..RequestFilter::default()
                },
            )
            .await;
        match rows {
            Ok(rows) => {
                let now = self.now();
                let pauses = rows
                    .iter()
                    .filter_map(|r| serde_json::from_str::<Pause>(&r.body).ok())
                    .filter(|p| p.holds(now))
                    .collect();
                self.state.operations.pauses.set(pauses);
            }
            Err(e) => tracing::debug!(error = %e, "ingress pauses could not be read"),
        }
    }

    /// The pauses that hold now.
    #[must_use]
    pub fn ingress_pauses(&self) -> Vec<Pause> {
        self.state.operations.pauses.active(self.now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pause(org: Option<&str>, until: i64) -> Pause {
        Pause {
            organization: org.map(str::to_owned),
            reason: "incident".into(),
            paused_by: "alice".into(),
            paused_at_us: 0,
            resumes_at_us: until,
        }
    }

    #[test]
    fn a_pause_refuses_its_scope_until_it_ends() {
        let p = Pauses::default();
        assert_eq!(p.refusal(UtcMicros(0), None), None);
        p.set(vec![pause(None, 2_000_000), pause(Some("acme"), 5_000_000)]);
        assert_eq!(
            p.refusal(UtcMicros(500_000), None),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            p.refusal(UtcMicros(3_000_000), Some("acme")),
            Some(Duration::from_secs(2))
        );
        assert_eq!(p.refusal(UtcMicros(3_000_000), None), None);
        assert_eq!(p.refusal(UtcMicros(0), Some("other")), None);
        let active = p.active(UtcMicros(0));
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].organization, None);
        p.clear();
        assert!(p.active(UtcMicros(0)).is_empty());
    }

    #[test]
    fn ids_and_representation() {
        assert_eq!(Pause::id_of(None), "gateway");
        assert_eq!(Pause::id_of(Some("acme")), "organization/acme");
        let shown = pause(Some("acme"), 60_000_000).shown();
        assert_eq!(shown["organization"], "acme");
        assert_eq!(shown["resumes_at"], "1970-01-01T00:01:00Z");
    }
}
