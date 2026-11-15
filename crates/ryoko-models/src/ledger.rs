use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::types::{ModelError, Pricing, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// USD cap; `f64::INFINITY` means no dollar cap.
    pub limit_usd: f64,
    #[serde(default)]
    pub limit_tokens: Option<u64>,
    #[serde(default)]
    pub limit_seconds: Option<u64>,
}

impl Budget {
    pub fn new(limit_usd: f64) -> Self {
        Self {
            limit_usd,
            limit_tokens: None,
            limit_seconds: None,
        }
    }

    pub fn unlimited() -> Self {
        Self::new(f64::INFINITY)
    }

    pub fn with_token_limit(mut self, limit_tokens: u64) -> Self {
        self.limit_tokens = Some(limit_tokens);
        self
    }

    pub fn with_time_limit(mut self, limit: Duration) -> Self {
        self.limit_seconds = Some(limit.as_secs());
        self
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub requests: u64,
    pub usage: Usage,
    pub cost_usd: f64,
}

impl UsageTotals {
    fn add_usage(&mut self, usage: Usage, cost_usd: f64) {
        self.requests += 1;
        self.usage += usage;
        self.cost_usd += cost_usd;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LedgerSnapshot {
    pub total: UsageTotals,
    pub by_provider: BTreeMap<String, UsageTotals>,
    pub by_model: BTreeMap<String, UsageTotals>,
    pub budget: Option<Budget>,
    pub budget_exceeded: bool,
    #[serde(default)]
    pub elapsed_ms: u64,
}

#[derive(Debug, Default)]
struct LedgerInner {
    total: UsageTotals,
    by_provider: BTreeMap<String, UsageTotals>,
    by_model: BTreeMap<String, UsageTotals>,
}

/// Single source of truth for token expenditure across every agent in a run.
///
/// Cheap to clone: all clones share the same counters and start time.
#[derive(Debug, Clone)]
pub struct UsageLedger {
    inner: Arc<Mutex<LedgerInner>>,
    budget: Option<Budget>,
    started_at: Instant,
}

impl Default for UsageLedger {
    fn default() -> Self {
        Self::new(None)
    }
}

impl UsageLedger {
    pub fn new(budget: Option<Budget>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LedgerInner::default())),
            budget,
            started_at: Instant::now(),
        }
    }

    pub fn with_budget(limit_usd: f64) -> Self {
        Self::new(Some(Budget::new(limit_usd)))
    }

    /// Records a completed model call and returns its computed cost.
    pub fn record(
        &self,
        provider: &str,
        model: &str,
        usage: Usage,
        pricing: Option<Pricing>,
    ) -> f64 {
        let cost = pricing.map(|p| p.cost_usd(&usage)).unwrap_or(0.0);
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .by_provider
            .entry(provider.to_owned())
            .or_default()
            .add_usage(usage, cost);
        inner
            .by_model
            .entry(model.to_owned())
            .or_default()
            .add_usage(usage, cost);
        inner.total.add_usage(usage, cost);
        cost
    }

    pub fn spent_usd(&self) -> f64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .total
            .cost_usd
    }

    pub fn spent_tokens(&self) -> u64 {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .total
            .usage
            .total_tokens()
    }

    pub fn elapsed(&self) -> Duration {
        self.started_at.elapsed()
    }

    pub fn budget(&self) -> Option<Budget> {
        self.budget
    }

    pub fn budget_exceeded(&self) -> bool {
        self.check_budget().is_err()
    }

    /// Called before dispatching a request and again after it completes so a
    /// runaway agent cannot keep spending once a cap is reached.
    pub fn check_budget(&self) -> Result<(), ModelError> {
        let Some(budget) = self.budget else {
            return Ok(());
        };
        let spent_usd = self.spent_usd();
        if spent_usd >= budget.limit_usd {
            return Err(ModelError::BudgetExceeded {
                budget_usd: budget.limit_usd,
                spent_usd,
            });
        }
        if let Some(limit_tokens) = budget.limit_tokens {
            let spent_tokens = self.spent_tokens();
            if spent_tokens >= limit_tokens {
                return Err(ModelError::TokenBudgetExceeded {
                    budget_tokens: limit_tokens,
                    spent_tokens,
                });
            }
        }
        if let Some(limit_seconds) = budget.limit_seconds {
            let elapsed = self.elapsed();
            if elapsed >= Duration::from_secs(limit_seconds) {
                return Err(ModelError::LatencyBudgetExceeded {
                    budget_secs: limit_seconds,
                    elapsed_ms: elapsed.as_millis() as u64,
                });
            }
        }
        Ok(())
    }

    /// Time left before the wall-clock cap, if one is configured.
    pub fn remaining_time(&self) -> Option<Duration> {
        let seconds = self.budget?.limit_seconds?;
        Some(Duration::from_secs(seconds).saturating_sub(self.elapsed()))
    }

    pub fn snapshot(&self) -> LedgerSnapshot {
        // Compute this before taking the lock: check_budget reads the counters.
        let budget_exceeded = self.budget_exceeded();
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        LedgerSnapshot {
            total: inner.total.clone(),
            by_provider: inner.by_provider.clone(),
            by_model: inner.by_model.clone(),
            budget: self.budget,
            budget_exceeded,
            elapsed_ms: self.elapsed().as_millis() as u64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_cost_per_provider_and_model() {
        let ledger = UsageLedger::new(None);
        let pricing = Pricing::new(3.0, 15.0).with_cached(0.3);

        ledger.record(
            "anthropic",
            "claude-sonnet",
            Usage {
                input_tokens: 1_000,
                output_tokens: 500,
                cached_input_tokens: 400,
            },
            Some(pricing),
        );
        ledger.record(
            "anthropic",
            "claude-sonnet",
            Usage::new(2_000, 1_000),
            Some(pricing),
        );

        let snapshot = ledger.snapshot();
        assert_eq!(snapshot.total.requests, 2);
        assert_eq!(snapshot.total.usage.input_tokens, 3_000);
        assert_eq!(snapshot.total.usage.output_tokens, 1_500);
        assert_eq!(snapshot.by_provider.len(), 1);
        assert_eq!(snapshot.by_model.len(), 1);

        let expected = (600.0 * 3.0 + 400.0 * 0.3 + 500.0 * 15.0) / 1e6
            + (2_000.0 * 3.0 + 1_000.0 * 15.0) / 1e6;
        assert!((snapshot.total.cost_usd - expected).abs() < 1e-12);
    }

    #[test]
    fn enforces_dollar_budget() {
        let ledger = UsageLedger::with_budget(0.001);
        assert!(ledger.check_budget().is_ok());
        ledger.record(
            "mock",
            "mock-1",
            Usage::new(10_000, 10_000),
            Some(Pricing::new(1.0, 1.0)),
        );
        assert!(ledger.budget_exceeded());
        assert!(matches!(
            ledger.check_budget(),
            Err(ModelError::BudgetExceeded { .. })
        ));
    }

    #[test]
    fn enforces_token_budget() {
        let ledger = UsageLedger::new(Some(Budget::unlimited().with_token_limit(1_000)));
        ledger.record("mock", "mock-1", Usage::new(600, 500), None);
        assert!(matches!(
            ledger.check_budget(),
            Err(ModelError::TokenBudgetExceeded {
                budget_tokens: 1_000,
                spent_tokens: 1_100
            })
        ));
        assert_eq!(ledger.snapshot().total.usage.total_tokens(), 1_100);
    }

    #[test]
    fn enforces_wall_clock_budget() {
        let ledger = UsageLedger::new(Some(Budget::unlimited().with_time_limit(Duration::ZERO)));
        assert!(matches!(
            ledger.check_budget(),
            Err(ModelError::LatencyBudgetExceeded { budget_secs: 0, .. })
        ));
        assert_eq!(ledger.remaining_time(), Some(Duration::ZERO));
    }

    #[test]
    fn unlimited_budget_never_blocks() {
        let ledger = UsageLedger::new(Some(Budget::unlimited()));
        ledger.record("mock", "mock-1", Usage::new(1_000_000, 1_000_000), None);
        assert!(ledger.check_budget().is_ok());
        assert!(!ledger.budget_exceeded());
    }
}
