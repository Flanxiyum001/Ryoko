use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::types::{ModelError, Pricing, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    /// Overall run cap; `f64::INFINITY` means no dollar cap.
    pub limit_usd: f64,
    #[serde(default)]
    pub limit_tokens: Option<u64>,
    #[serde(default)]
    pub limit_seconds: Option<u64>,
    /// Cap applied independently to each DAG task scope.
    #[serde(default)]
    pub task_limit_usd: Option<f64>,
    #[serde(default)]
    pub task_limit_tokens: Option<u64>,
    #[serde(default)]
    pub task_limit_seconds: Option<u64>,
}

impl Budget {
    pub fn new(limit_usd: f64) -> Self {
        Self {
            limit_usd,
            limit_tokens: None,
            limit_seconds: None,
            task_limit_usd: None,
            task_limit_tokens: None,
            task_limit_seconds: None,
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

    pub fn with_task_usd_limit(mut self, limit_usd: f64) -> Self {
        self.task_limit_usd = Some(limit_usd);
        self
    }

    pub fn with_task_token_limit(mut self, limit_tokens: u64) -> Self {
        self.task_limit_tokens = Some(limit_tokens);
        self
    }

    pub fn with_task_time_limit(mut self, limit: Duration) -> Self {
        self.task_limit_seconds = Some(limit.as_secs());
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
    /// Usage attributed to each DAG task by request scope.
    #[serde(default)]
    pub by_task: BTreeMap<String, UsageTotals>,
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
    by_task: BTreeMap<String, UsageTotals>,
    task_started: BTreeMap<String, Instant>,
    reservations: BTreeMap<u64, (Option<String>, f64, u64)>,
    next_reservation: u64,
}

/// A pending call owns its allowance until usage is recorded or the stream is
/// dropped. Clones of the ledger see the same pending reservations.
pub struct Reservation {
    ledger: UsageLedger,
    id: u64,
    settled: bool,
}

impl Reservation {
    pub fn settle(mut self, provider: &str, model: &str, usage: Usage, pricing: Option<Pricing>) {
        let mut inner = self.ledger.inner.lock().unwrap_or_else(|e| e.into_inner());
        let scope = inner
            .reservations
            .remove(&self.id)
            .and_then(|(scope, _, _)| scope);
        record_locked(
            &mut inner,
            scope.as_deref(),
            provider,
            model,
            usage,
            pricing,
        );
        self.settled = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.settled {
            let mut inner = self.ledger.inner.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((scope, cost, tokens)) = inner.reservations.remove(&self.id) {
                // An interrupted provider may still bill without sending a
                // usage event. Consume its whole allowance conservatively.
                let estimated = Usage::new(tokens, 0);
                inner.total.add_usage(estimated, cost);
                if let Some(scope) = scope {
                    inner
                        .by_task
                        .entry(scope)
                        .or_default()
                        .add_usage(estimated, cost);
                }
            }
        }
    }
}

fn record_locked(
    inner: &mut LedgerInner,
    scope: Option<&str>,
    provider: &str,
    model: &str,
    usage: Usage,
    pricing: Option<Pricing>,
) -> f64 {
    let cost = pricing.map(|p| p.cost_usd(&usage)).unwrap_or(0.0);
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
    if let Some(scope) = scope {
        inner
            .by_task
            .entry(scope.to_owned())
            .or_default()
            .add_usage(usage, cost);
        inner
            .task_started
            .entry(scope.to_owned())
            .or_insert_with(Instant::now);
    }
    cost
}

/// Single source of truth for token expenditure across every agent in a run.
///
/// Cheap to clone: all clones share the same counters and start time. Calls
/// carrying a task scope are also accounted against that task's own caps.
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

    /// Records a completed model call without task attribution.
    pub fn record(
        &self,
        provider: &str,
        model: &str,
        usage: Usage,
        pricing: Option<Pricing>,
    ) -> f64 {
        self.record_for(None, provider, model, usage, pricing)
    }

    /// Records a completed model call, optionally against a DAG task scope.
    pub fn record_for(
        &self,
        scope: Option<&str>,
        provider: &str,
        model: &str,
        usage: Usage,
        pricing: Option<Pricing>,
    ) -> f64 {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        record_locked(&mut inner, scope, provider, model, usage, pricing)
    }

    /// Atomically reserve the maximum allowed cost and tokens before dispatch.
    /// Calls without a finite price or output cap cannot be admitted under a
    /// bounded budget.
    pub fn reserve(
        &self,
        scope: Option<&str>,
        cost: f64,
        tokens: u64,
    ) -> Result<Option<Reservation>, ModelError> {
        let Some(budget) = self.budget else {
            return Ok(None);
        };
        self.check_budget_for(scope)?;
        if !cost.is_finite() || cost < 0.0 || tokens == 0 {
            return Err(ModelError::Configuration(
                "cannot reserve an unbounded model call".to_owned(),
            ));
        }
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (pending_usd, pending_tokens) = inner
            .reservations
            .values()
            .fold((0.0, 0u64), |(usd, tokens), (_, cost, count)| {
                (usd + cost, tokens.saturating_add(*count))
            });
        let projected = inner.total.cost_usd + pending_usd + cost;
        if projected > budget.limit_usd {
            return Err(ModelError::BudgetExceeded {
                budget_usd: budget.limit_usd,
                spent_usd: projected,
            });
        }
        if let Some(limit) = budget.limit_tokens {
            let projected = inner
                .total
                .usage
                .total_tokens()
                .saturating_add(pending_tokens)
                .saturating_add(tokens);
            if projected > limit {
                return Err(ModelError::TokenBudgetExceeded {
                    budget_tokens: limit,
                    spent_tokens: projected,
                });
            }
        }
        if let Some(scope) = scope {
            let (pending_usd, pending_tokens) = inner
                .reservations
                .values()
                .filter(|(task, _, _)| task.as_deref() == Some(scope))
                .fold((0.0, 0u64), |(usd, tokens), (_, cost, count)| {
                    (usd + cost, tokens.saturating_add(*count))
                });
            let spent = inner.by_task.get(scope).cloned().unwrap_or_default();
            let projected = spent.cost_usd + pending_usd + cost;
            if let Some(limit) = budget.task_limit_usd
                && projected > limit
            {
                return Err(ModelError::TaskBudgetExceeded {
                    task: scope.to_owned(),
                    budget_usd: limit,
                    spent_usd: projected,
                });
            }
            if let Some(limit) = budget.task_limit_tokens {
                let projected = spent
                    .usage
                    .total_tokens()
                    .saturating_add(pending_tokens)
                    .saturating_add(tokens);
                if projected > limit {
                    return Err(ModelError::TaskTokenBudgetExceeded {
                        task: scope.to_owned(),
                        budget_tokens: limit,
                        spent_tokens: projected,
                    });
                }
            }
        }
        inner.next_reservation = inner
            .next_reservation
            .checked_add(1)
            .ok_or_else(|| ModelError::Configuration("reservation counter exhausted".to_owned()))?;
        let id = inner.next_reservation;
        inner
            .reservations
            .insert(id, (scope.map(str::to_owned), cost, tokens));
        Ok(Some(Reservation {
            ledger: self.clone(),
            id,
            settled: false,
        }))
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

    /// Checks the overall run budget. Task-specific checks use
    /// [`check_budget_for`](Self::check_budget_for).
    pub fn check_budget(&self) -> Result<(), ModelError> {
        self.check_budget_for(None)
    }

    /// Checks the overall run caps first, then the caps for `scope` when one is
    /// supplied. Task wall-clock time starts at the first check or recorded call
    /// for that scope.
    pub fn check_budget_for(&self, scope: Option<&str>) -> Result<(), ModelError> {
        let Some(budget) = self.budget else {
            return Ok(());
        };

        let total = self
            .inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .total
            .clone();
        if total.cost_usd >= budget.limit_usd {
            return Err(ModelError::BudgetExceeded {
                budget_usd: budget.limit_usd,
                spent_usd: total.cost_usd,
            });
        }
        if let Some(limit_tokens) = budget.limit_tokens {
            let spent_tokens = total.usage.total_tokens();
            if spent_tokens >= limit_tokens {
                return Err(ModelError::TokenBudgetExceeded {
                    budget_tokens: limit_tokens,
                    spent_tokens,
                });
            }
        }
        if let Some(limit_seconds) = budget.limit_seconds
            && self.elapsed() >= Duration::from_secs(limit_seconds)
        {
            return Err(ModelError::LatencyBudgetExceeded {
                budget_secs: limit_seconds,
                elapsed_ms: self.elapsed().as_millis() as u64,
            });
        }

        let Some(scope) = scope else {
            return Ok(());
        };
        let (task, elapsed) = {
            let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            let started = *inner
                .task_started
                .entry(scope.to_owned())
                .or_insert_with(Instant::now);
            let task = inner.by_task.get(scope).cloned().unwrap_or_default();
            (task, started.elapsed())
        };
        if let Some(limit_usd) = budget.task_limit_usd
            && task.cost_usd >= limit_usd
        {
            return Err(ModelError::TaskBudgetExceeded {
                task: scope.to_owned(),
                budget_usd: limit_usd,
                spent_usd: task.cost_usd,
            });
        }
        if let Some(limit_tokens) = budget.task_limit_tokens {
            let spent_tokens = task.usage.total_tokens();
            if spent_tokens >= limit_tokens {
                return Err(ModelError::TaskTokenBudgetExceeded {
                    task: scope.to_owned(),
                    budget_tokens: limit_tokens,
                    spent_tokens,
                });
            }
        }
        if let Some(limit_seconds) = budget.task_limit_seconds
            && elapsed >= Duration::from_secs(limit_seconds)
        {
            return Err(ModelError::TaskLatencyBudgetExceeded {
                task: scope.to_owned(),
                budget_secs: limit_seconds,
                elapsed_ms: elapsed.as_millis() as u64,
            });
        }
        Ok(())
    }

    /// Overall time left before the wall-clock cap, if one is configured.
    pub fn remaining_time(&self) -> Option<Duration> {
        let seconds = self.budget?.limit_seconds?;
        Some(Duration::from_secs(seconds).saturating_sub(self.elapsed()))
    }

    /// Attribute a deadline-triggered timeout to the cap which constrained
    /// the request. A task may start well after the overall run clock.
    pub fn timeout_error(&self, scope: Option<&str>) -> ModelError {
        let run_remaining = self.remaining_time();
        if let (Some(scope), Some(limit_seconds)) = (
            scope,
            self.budget.and_then(|budget| budget.task_limit_seconds),
        ) {
            let started = self
                .inner
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .task_started
                .get(scope)
                .copied();
            if let Some(started) = started {
                let elapsed = started.elapsed();
                let task_remaining = Duration::from_secs(limit_seconds).saturating_sub(elapsed);
                if run_remaining.is_none_or(|run| task_remaining < run) {
                    return ModelError::TaskLatencyBudgetExceeded {
                        task: scope.to_owned(),
                        budget_secs: limit_seconds,
                        elapsed_ms: elapsed.as_millis() as u64,
                    };
                }
            }
        }
        ModelError::LatencyBudgetExceeded {
            budget_secs: self
                .budget
                .and_then(|budget| budget.limit_seconds)
                .unwrap_or(0),
            elapsed_ms: self.elapsed().as_millis() as u64,
        }
    }

    /// Shortest remaining time across the run and task caps.
    pub fn remaining_time_for(&self, scope: Option<&str>) -> Option<Duration> {
        let run = self.remaining_time();
        let task = scope.and_then(|scope| {
            let seconds = self.budget?.task_limit_seconds?;
            let started = self
                .inner
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .task_started
                .get(scope)
                .copied()?;
            Some(Duration::from_secs(seconds).saturating_sub(started.elapsed()))
        });
        match (run, task) {
            (Some(run), Some(task)) => Some(run.min(task)),
            (run, task) => run.or(task),
        }
    }

    pub fn snapshot(&self) -> LedgerSnapshot {
        // Compute this before taking the lock: check_budget reads the counters.
        let budget_exceeded = self.budget_exceeded();
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        LedgerSnapshot {
            total: inner.total.clone(),
            by_provider: inner.by_provider.clone(),
            by_model: inner.by_model.clone(),
            by_task: inner.by_task.clone(),
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
    fn concurrent_reservations_cannot_overbook_run_or_task() {
        let ledger = UsageLedger::new(Some(Budget::new(1.0).with_task_usd_limit(0.6)));
        let first = ledger.reserve(Some("a"), 0.5, 100).unwrap().unwrap();
        assert!(matches!(
            ledger.reserve(Some("a"), 0.2, 100),
            Err(ModelError::TaskBudgetExceeded { .. })
        ));
        let second = ledger.reserve(Some("b"), 0.5, 100).unwrap().unwrap();
        assert!(matches!(
            ledger.reserve(Some("c"), 0.1, 100),
            Err(ModelError::BudgetExceeded { .. })
        ));
        first.settle(
            "mock",
            "model",
            Usage::new(100, 0),
            Some(Pricing::new(0.0, 0.0)),
        );
        second.settle(
            "mock",
            "model",
            Usage::new(100, 0),
            Some(Pricing::new(1.0, 1.0)),
        );
        assert!(ledger.reserve(Some("a"), 0.5, 100).is_ok());
        assert_eq!(ledger.snapshot().by_task["b"].requests, 1);
    }

    #[test]
    fn missing_usage_conservatively_consumes_allowance() {
        let ledger = UsageLedger::new(Some(Budget::unlimited().with_token_limit(100)));
        let first = ledger.reserve(None, 0.0, 70).unwrap().unwrap();
        assert!(matches!(
            ledger.reserve(None, 0.0, 40),
            Err(ModelError::TokenBudgetExceeded { .. })
        ));
        drop(first);
        assert!(matches!(
            ledger.reserve(None, 0.0, 40),
            Err(ModelError::TokenBudgetExceeded { .. })
        ));
    }

    #[test]
    fn parallel_admission_is_atomic() {
        let ledger = UsageLedger::with_budget(1.0);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let handles = (0..2)
            .map(|_| {
                let ledger = ledger.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let reservation = ledger.reserve(None, 0.7, 100);
                    barrier.wait();
                    reservation.is_ok()
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        barrier.wait();
        assert_eq!(
            handles
                .into_iter()
                .filter(|handle| handle.join().unwrap())
                .count(),
            1
        );
    }

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

    #[test]
    fn task_caps_are_isolated_and_do_not_breach_the_run_cap() {
        let ledger = UsageLedger::new(Some(
            Budget::unlimited()
                .with_token_limit(10_000)
                .with_task_token_limit(1_000),
        ));
        ledger.record_for(Some("task-a"), "mock", "mock-1", Usage::new(600, 500), None);
        ledger.record_for(Some("task-b"), "mock", "mock-1", Usage::new(100, 100), None);

        assert!(ledger.check_budget_for(Some("task-b")).is_ok());
        assert!(matches!(
            ledger.check_budget_for(Some("task-a")),
            Err(ModelError::TaskTokenBudgetExceeded {
                task,
                spent_tokens: 1_100,
                ..
            }) if task == "task-a"
        ));
        assert_eq!(
            ledger.snapshot().by_task["task-a"].usage.total_tokens(),
            1_100
        );
        assert_eq!(
            ledger.snapshot().by_task["task-b"].usage.total_tokens(),
            200
        );
    }

    #[test]
    fn task_dollar_cap_and_overall_run_cap_are_both_enforced() {
        let ledger = UsageLedger::new(Some(Budget::new(100.0).with_task_usd_limit(0.001)));
        ledger.record_for(
            Some("task-a"),
            "mock",
            "mock-1",
            Usage::new(10_000, 10_000),
            Some(Pricing::new(1.0, 1.0)),
        );
        assert!(matches!(
            ledger.check_budget_for(Some("task-a")),
            Err(ModelError::TaskBudgetExceeded { task, .. }) if task == "task-a"
        ));
        assert!(ledger.check_budget_for(Some("task-b")).is_ok());
    }

    #[test]
    fn timeout_error_attributes_the_binding_cap() {
        let task_only = UsageLedger::new(Some(
            Budget::unlimited().with_task_time_limit(Duration::from_secs(11)),
        ));
        task_only.check_budget_for(Some("task-a")).unwrap();
        assert!(matches!(
            task_only.timeout_error(Some("task-a")),
            ModelError::TaskLatencyBudgetExceeded {
                task,
                budget_secs: 11,
                ..
            } if task == "task-a"
        ));

        let run_only = UsageLedger::new(Some(
            Budget::unlimited().with_time_limit(Duration::from_secs(12)),
        ));
        assert!(matches!(
            run_only.timeout_error(Some("task-a")),
            ModelError::LatencyBudgetExceeded {
                budget_secs: 12,
                ..
            }
        ));

        let task_tighter = UsageLedger::new(Some(
            Budget::unlimited()
                .with_time_limit(Duration::from_secs(30))
                .with_task_time_limit(Duration::from_secs(8)),
        ));
        task_tighter.check_budget_for(Some("task-a")).unwrap();
        assert!(matches!(
            task_tighter.timeout_error(Some("task-a")),
            ModelError::TaskLatencyBudgetExceeded {
                task,
                budget_secs: 8,
                ..
            } if task == "task-a"
        ));

        let run_tighter = UsageLedger::new(Some(
            Budget::unlimited()
                .with_time_limit(Duration::from_secs(5))
                .with_task_time_limit(Duration::from_secs(20)),
        ));
        run_tighter.check_budget_for(Some("task-a")).unwrap();
        assert!(matches!(
            run_tighter.timeout_error(Some("task-a")),
            ModelError::LatencyBudgetExceeded { budget_secs: 5, .. }
        ));
    }

    #[test]
    fn task_wall_clock_cap_is_tracked_per_scope() {
        let ledger = UsageLedger::new(Some(
            Budget::unlimited().with_task_time_limit(Duration::ZERO),
        ));
        assert!(matches!(
            ledger.check_budget_for(Some("task-a")),
            Err(ModelError::TaskLatencyBudgetExceeded { task, .. }) if task == "task-a"
        ));
        assert_eq!(
            ledger.remaining_time_for(Some("task-a")),
            Some(Duration::ZERO)
        );
    }
}
