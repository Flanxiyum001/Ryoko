//! Durable, deterministic task DAG planning.
//!
//! This coordinator plans and records state; it never executes commands,
//! calls models, approves itself or merges changes.

use std::collections::{BTreeSet, HashMap, HashSet};

use ryoko_storage::{DurableStore, StorageError, TaskRecord, TaskState};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_TASKS: usize = 128;
pub const MAX_DEPENDENCIES: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("invalid task graph: {0}")]
    InvalidGraph(String),
    #[error("task {0} is not ready")]
    NotReady(Uuid),
    #[error("durable state error: {0}")]
    Storage(#[from] StorageError),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedTask {
    pub id: Uuid,
    pub goal: String,
    pub dependencies: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskGraph {
    pub tasks: Vec<PlannedTask>,
}

impl TaskGraph {
    /// Rejects duplicate IDs, missing dependencies, cycles and oversized plans.
    /// Returns a deterministic dependency-first ordering.
    pub fn validate(&self) -> Result<Vec<Uuid>, PlanError> {
        if self.tasks.is_empty() || self.tasks.len() > MAX_TASKS {
            return Err(PlanError::InvalidGraph(
                "graph must contain between 1 and 128 tasks".into(),
            ));
        }

        let mut nodes = HashMap::new();
        for task in &self.tasks {
            if task.goal.trim().is_empty() || task.goal.len() > 16_384 {
                return Err(PlanError::InvalidGraph("invalid task goal".into()));
            }
            if task.dependencies.len() > MAX_DEPENDENCIES {
                return Err(PlanError::InvalidGraph("too many dependencies".into()));
            }
            if nodes.insert(task.id, task).is_some() {
                return Err(PlanError::InvalidGraph("duplicate task ID".into()));
            }
        }

        let mut degrees = HashMap::new();
        let mut dependents: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for task in &self.tasks {
            let mut unique = HashSet::new();
            for dependency in &task.dependencies {
                if *dependency == task.id
                    || !nodes.contains_key(dependency)
                    || !unique.insert(*dependency)
                {
                    return Err(PlanError::InvalidGraph(
                        "unknown, duplicate or self dependency".into(),
                    ));
                }
                dependents.entry(*dependency).or_default().push(task.id);
            }
            degrees.insert(task.id, task.dependencies.len());
        }

        let mut ready = degrees
            .iter()
            .filter(|(_, count)| **count == 0)
            .map(|(id, _)| *id)
            .collect::<BTreeSet<_>>();
        let mut order = Vec::with_capacity(self.tasks.len());
        while let Some(id) = ready.pop_first() {
            order.push(id);
            if let Some(children) = dependents.get(&id) {
                for child in children {
                    let degree = degrees.get_mut(child).ok_or_else(|| {
                        PlanError::InvalidGraph("unknown dependency child".into())
                    })?;
                    *degree -= 1;
                    if *degree == 0 {
                        ready.insert(*child);
                    }
                }
            }
        }
        if order.len() != self.tasks.len() {
            return Err(PlanError::InvalidGraph("dependency cycle".into()));
        }
        Ok(order)
    }
}

pub struct DurablePlanner {
    graph: TaskGraph,
    order: Vec<Uuid>,
    store: DurableStore,
}

impl DurablePlanner {
    pub fn new(graph: TaskGraph, mut store: DurableStore) -> Result<Self, PlanError> {
        let order = graph.validate()?;
        for id in &order {
            let task = graph
                .tasks
                .iter()
                .find(|task| task.id == *id)
                .ok_or_else(|| PlanError::InvalidGraph("missing node".into()))?;
            if let Some(previous) = store.task(*id)? {
                if previous.goal != task.goal {
                    return Err(PlanError::InvalidGraph(
                        "reused task ID has different goal".into(),
                    ));
                }
            } else {
                store.create_task(*id, &task.goal)?;
            }
        }
        Ok(Self {
            graph,
            order,
            store,
        })
    }

    /// The only runnable tasks are queued tasks whose dependencies are
    /// completed. A failed or cancelled parent does not unblock children.
    pub fn ready(&self) -> Result<Vec<Uuid>, PlanError> {
        let mut ready = Vec::new();
        for id in &self.order {
            let Some(task) = self.store.task(*id)? else {
                continue;
            };
            if task.state != TaskState::Queued {
                continue;
            }
            let Some(node) = self.graph.tasks.iter().find(|node| node.id == *id) else {
                continue;
            };
            let mut dependencies_complete = true;
            for dependency in &node.dependencies {
                let state = self.store.task(*dependency)?;
                if !state.is_some_and(|value| value.state == TaskState::Completed) {
                    dependencies_complete = false;
                    break;
                }
            }
            if dependencies_complete {
                ready.push(*id);
            }
        }
        Ok(ready)
    }

    /// Mark a task as running only when all prerequisites are completed.
    /// This does not trigger model or tool execution.
    pub fn begin(&mut self, id: Uuid, revision: u64) -> Result<TaskRecord, PlanError> {
        if !self.ready()?.contains(&id) {
            return Err(PlanError::NotReady(id));
        }
        Ok(self.store.transition(id, revision, TaskState::Running)?)
    }

    /// The caller must provide the outcome of validation explicitly.
    pub fn finish(
        &mut self,
        id: Uuid,
        revision: u64,
        validation_passed: bool,
    ) -> Result<TaskRecord, PlanError> {
        let next = if validation_passed {
            TaskState::Completed
        } else {
            TaskState::Failed
        };
        Ok(self.store.transition(id, revision, next)?)
    }

    pub fn pause_for_approval(&mut self, id: Uuid, revision: u64) -> Result<TaskRecord, PlanError> {
        Ok(self
            .store
            .transition(id, revision, TaskState::AwaitingApproval)?)
    }

    /// Only a human-facing caller may explicitly provide this decision.
    pub fn record_approval(
        &mut self,
        id: Uuid,
        revision: u64,
        approved: bool,
    ) -> Result<TaskRecord, PlanError> {
        let next = if approved {
            TaskState::Running
        } else {
            TaskState::Cancelled
        };
        Ok(self.store.transition(id, revision, next)?)
    }

    /// Read-only crash-recovery list. No task is resumed automatically.
    pub fn interrupted(&self) -> Result<Vec<TaskRecord>, PlanError> {
        Ok(self.store.interrupted_tasks()?)
    }

    pub fn task(&self, id: Uuid) -> Result<Option<TaskRecord>, PlanError> {
        Ok(self.store.task(id)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> (TaskGraph, Uuid, Uuid) {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        (
            TaskGraph {
                tasks: vec![
                    PlannedTask {
                        id: second,
                        goal: "Validate the change".into(),
                        dependencies: vec![first],
                    },
                    PlannedTask {
                        id: first,
                        goal: "Implement the change".into(),
                        dependencies: vec![],
                    },
                ],
            },
            first,
            second,
        )
    }

    #[test]
    fn rejects_cycles_and_unknown_dependencies() {
        let (mut plan, first, second) = graph();
        assert_eq!(plan.validate().unwrap(), vec![first, second]);
        plan.tasks[1].dependencies.push(second);
        assert!(plan.validate().is_err());
        plan.tasks[1].dependencies.clear();
        plan.tasks[0].dependencies = vec![Uuid::new_v4()];
        assert!(plan.validate().is_err());
    }

    #[test]
    fn only_unblocks_children_after_verified_completion_and_recovers_state() {
        let (graph, first, second) = graph();
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("tasks.sqlite3");
        let store = DurableStore::open(&path).unwrap();
        let mut planner = DurablePlanner::new(graph.clone(), store).unwrap();
        assert_eq!(planner.ready().unwrap(), vec![first]);
        assert!(matches!(
            planner.begin(second, 0),
            Err(PlanError::NotReady(_))
        ));
        let begun = planner.begin(first, 0).unwrap();
        assert_eq!(begun.revision, 1);
        assert!(planner.ready().unwrap().is_empty());
        planner.pause_for_approval(first, 1).unwrap();
        assert_eq!(planner.interrupted().unwrap().len(), 1);
        planner.record_approval(first, 2, true).unwrap();
        planner.finish(first, 3, true).unwrap();
        assert_eq!(planner.ready().unwrap(), vec![second]);
        drop(planner);

        let store = DurableStore::open(&path).unwrap();
        let mut recovered = DurablePlanner::new(graph, store).unwrap();
        assert_eq!(recovered.ready().unwrap(), vec![second]);
        recovered.begin(second, 0).unwrap();
        recovered.finish(second, 1, false).unwrap();
        assert!(recovered.ready().unwrap().is_empty());
    }

    #[test]
    fn denies_implicit_approval_and_stale_transitions() {
        let (graph, first, _) = graph();
        let temp = tempfile::TempDir::new().unwrap();
        let store = DurableStore::open(&temp.path().join("tasks.sqlite3")).unwrap();
        let mut planner = DurablePlanner::new(graph, store).unwrap();
        planner.begin(first, 0).unwrap();
        planner.pause_for_approval(first, 1).unwrap();
        assert!(planner.finish(first, 2, true).is_err());
        assert!(planner.record_approval(first, 1, true).is_err());
        planner.record_approval(first, 2, false).unwrap();
        assert_eq!(
            planner.task(first).unwrap().unwrap().state,
            TaskState::Cancelled
        );
    }
}
