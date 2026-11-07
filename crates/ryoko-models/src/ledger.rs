use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::types::{ModelError, Pricing, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub limit_usd: f64,
}

impl Budget {
    pub fn new(limit_usd: f64) -> Self {
        Self { limit_usd }
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
}

#[derive(Debug, Default)]
struct LedgerInner {
    total: UsageTotals,
    by_provider: BTreeMap<String, UsageTotals>,
    by_model: BTreeMap<String, UsageTotals>,
}

/// Single source of truth for token expenditure across every agent in a run.
///
/// Cheap to clone: all clones share the same counters.
#[derive(Debug, Clone, Default)]
pub struct UsageLedger {
    inner: Arc<Mutex<LedgerInner>>,
    budget: Option<Budget>,
}

impl UsageLedger {
    pub fn new(budget: Option<Budget>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(LedgerInner::default())),
            budget,
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

    pub fn budget(&self) -> Option<Budget> {
        self.budget
    }

    pub fn budget_exceeded(&self) -> bool {
        self.budget
            .map(|b| self.spent_usd() >= b.limit_usd)
            .unwrap_or(false)
    }

    /// Called before dispatching a request so a runaway agent cannot keep
    /// spending once the cap is reached.
    pub fn check_budget(&self) -> Result<(), ModelError> {
        if let Some(budget) = self.budget
            && self.spent_usd() >= budget.limit_usd
        {
            return Err(ModelError::BudgetExceeded {
                budget_usd: budget.limit_usd,
                spent_usd: self.spent_usd(),
            });
        }
        Ok(())
    }

    pub fn snapshot(&self) -> LedgerSnapshot {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        LedgerSnapshot {
            total: inner.total.clone(),
            by_provider: inner.by_provider.clone(),
            by_model: inner.by_model.clone(),
            budget: self.budget,
            budget_exceeded: self.budget_exceeded(),
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
    fn enforces_budget() {
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
}
