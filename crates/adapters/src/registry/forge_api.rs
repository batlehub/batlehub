//! What the GitHub and Forgejo clients share for RFC 0019: the rate-limit
//! budget around every API call, and the date parsing the forges' JSON needs.
//!
//! The budget is consulted *by the client*, because the client is the only
//! party that sees `X-RateLimit-*` come back. Every API request goes through
//! [`BudgetedApi::send`]: refused below the role's reserve without a call,
//! and every answer's headers are recorded for the next caller — on this
//! process or on the worker that shares the token.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use batlehub_core::error::CoreError;
use batlehub_core::ports::{BudgetRole, RateLimitBudget, RateLimitObservation};

/// The budget a client draws on: the shared store plus the row it reads.
#[derive(Clone)]
pub struct BudgetedApi {
    budget: Option<Arc<dyn RateLimitBudget>>,
    registry: String,
    token_fingerprint: String,
}

impl BudgetedApi {
    /// No budget: every call goes through and nothing is recorded — the shape
    /// a client has until the server hands it one.
    pub fn unbudgeted() -> Self {
        Self {
            budget: None,
            registry: String::new(),
            token_fingerprint: "anonymous".to_owned(),
        }
    }

    pub fn new(
        budget: Arc<dyn RateLimitBudget>,
        registry: impl Into<String>,
        token_fingerprint: impl Into<String>,
    ) -> Self {
        Self {
            budget: Some(budget),
            registry: registry.into(),
            token_fingerprint: token_fingerprint.into(),
        }
    }

    /// Send an API request as `role`, refusing below the role's reserve and
    /// recording the forge's rate-limit headers from the answer.
    pub async fn send(
        &self,
        req: reqwest::RequestBuilder,
        role: BudgetRole,
    ) -> Result<reqwest::Response, CoreError> {
        if let Some(budget) = &self.budget {
            let allowed = budget
                .acquire(&self.registry, &self.token_fingerprint, role)
                .await
                .unwrap_or_else(|e| {
                    // A store that cannot be read is not a reason to stop
                    // serving: the budget exists to keep the proxy alive.
                    tracing::warn!(error = %e, "rate-limit budget unreadable; allowing the call");
                    true
                });
            if !allowed {
                return Err(CoreError::Registry(format!(
                    "rate-limit budget for '{}' is below the {:?} reserve; not calling the forge \
                     until the window resets",
                    self.registry, role
                )));
            }
        }
        let resp = req
            .send()
            .await
            .map_err(|e| CoreError::Registry(e.to_string()))?;
        if let (Some(budget), Some(obs)) = (&self.budget, rate_limit_observation(resp.headers())) {
            if let Err(e) = budget
                .observe(&self.registry, &self.token_fingerprint, obs)
                .await
            {
                tracing::warn!(error = %e, "could not record the forge's rate-limit headers");
            }
        }
        Ok(resp)
    }
}

/// `X-RateLimit-Remaining`, `X-RateLimit-Limit` and `X-RateLimit-Reset` (a
/// Unix timestamp), when all three are present. GitHub sends them on every
/// API answer; Forgejo sends none, and a forge that says nothing is not
/// budgeted.
pub fn rate_limit_observation(
    headers: &reqwest::header::HeaderMap,
) -> Option<RateLimitObservation> {
    let int = |name: &str| -> Option<i64> { headers.get(name)?.to_str().ok()?.trim().parse().ok() };
    let remaining = int("x-ratelimit-remaining")?;
    let limit = int("x-ratelimit-limit")?;
    let reset = int("x-ratelimit-reset")?;
    let reset_at = DateTime::<Utc>::from_timestamp(reset, 0)?;
    Some(RateLimitObservation {
        remaining,
        limit,
        reset_at,
    })
}

/// An RFC 3339 date as the forges write it, or nothing.
pub fn parse_date(s: Option<&str>) -> Option<DateTime<Utc>> {
    s.and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// `login` when the forge has one for the person, else `name <email>`, else
/// whichever half exists.
pub fn person_label(
    login: Option<&str>,
    name: Option<&str>,
    email: Option<&str>,
) -> Option<String> {
    if let Some(l) = login.filter(|l| !l.is_empty()) {
        return Some(l.to_owned());
    }
    match (
        name.filter(|n| !n.is_empty()),
        email.filter(|e| !e.is_empty()),
    ) {
        (Some(n), Some(e)) => Some(format!("{n} <{e}>")),
        (Some(n), None) => Some(n.to_owned()),
        (None, Some(e)) => Some(e.to_owned()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_rate_limit_headers_are_read_and_a_missing_one_means_no_observation() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-ratelimit-remaining", "58".parse().unwrap());
        h.insert("x-ratelimit-limit", "60".parse().unwrap());
        h.insert("x-ratelimit-reset", "1788469320".parse().unwrap());
        let obs = rate_limit_observation(&h).unwrap();
        assert_eq!((obs.remaining, obs.limit), (58, 60));
        assert_eq!(obs.reset_at.timestamp(), 1788469320);

        h.remove("x-ratelimit-reset");
        assert!(rate_limit_observation(&h).is_none());
        assert!(rate_limit_observation(&reqwest::header::HeaderMap::new()).is_none());
    }

    #[test]
    fn people_are_named_by_login_first() {
        assert_eq!(
            person_label(Some("mfenniak"), Some("Mathieu"), Some("m@x")).as_deref(),
            Some("mfenniak")
        );
        assert_eq!(
            person_label(None, Some("Junio C Hamano"), Some("gitster@pobox.com")).as_deref(),
            Some("Junio C Hamano <gitster@pobox.com>")
        );
        assert_eq!(person_label(None, None, None), None);
    }
}
