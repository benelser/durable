//! Multi-agent coordination — durable coordinator/worker DAG.
//!
//! The coordinator defines a directed acyclic graph of workers. Workers
//! execute in topological order — same-level workers run in parallel.
//! The coordinator is itself a durable flow: on crash recovery, completed
//! workers are skipped and only incomplete workers re-run.

use crate::core::error::{DurableError, DurableResult};
use crate::core::hash::fnv1a_hash;
use crate::core::types::ExecutionId;
use crate::core::uuid::Uuid;
use crate::execution::replay::ReplayContext;
use crate::json;
use crate::storage::{EventStore, ExecutionLog};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

/// A worker in the coordination DAG.
pub struct WorkerDef {
    id: String,
    dependencies: Vec<String>,
    factory: Box<dyn FnOnce(HashMap<String, json::Value>) -> DurableResult<json::Value> + Send>,
}

/// Durable coordinator for parallel, dependency-aware worker execution.
///
/// Each worker is a child flow backed by the event log. On crash recovery,
/// completed workers return cached results; only incomplete workers re-run.
pub struct AgentCoordinator {
    workers: Vec<WorkerDef>,
    event_store: Arc<dyn EventStore>,
    #[allow(dead_code)]
    storage: Arc<dyn ExecutionLog>,
}

impl AgentCoordinator {
    pub fn new(event_store: Arc<dyn EventStore>, storage: Arc<dyn ExecutionLog>) -> Self {
        Self {
            workers: Vec::new(),
            event_store,
            storage,
        }
    }

    /// Register a worker with its dependencies.
    /// The closure receives a map of dependency results keyed by worker ID.
    pub fn add_worker<F>(&mut self, id: impl Into<String>, dependencies: Vec<String>, f: F)
    where
        F: FnOnce(HashMap<String, json::Value>) -> DurableResult<json::Value> + Send + 'static,
    {
        self.workers.push(WorkerDef {
            id: id.into(),
            dependencies,
            factory: Box::new(f),
        });
    }

    /// Validate the DAG (check for missing deps and cycles).
    pub fn validate(&self) -> Result<(), String> {
        let ids: BTreeSet<&str> = self.workers.iter().map(|w| w.id.as_str()).collect();

        for worker in &self.workers {
            for dep in &worker.dependencies {
                if !ids.contains(dep.as_str()) {
                    return Err(format!(
                        "worker '{}' depends on '{}' which doesn't exist",
                        worker.id, dep
                    ));
                }
            }
        }

        // Cycle detection via Kahn's algorithm
        let mut in_degree: HashMap<&str, usize> = HashMap::new();
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();

        for worker in &self.workers {
            in_degree.entry(worker.id.as_str()).or_insert(0);
            adj.entry(worker.id.as_str()).or_default();
            for dep in &worker.dependencies {
                *in_degree.entry(worker.id.as_str()).or_insert(0) += 1;
                adj.entry(dep.as_str()).or_default().push(worker.id.as_str());
            }
        }

        let mut queue: Vec<&str> = in_degree
            .iter()
            .filter(|(_, &deg)| deg == 0)
            .map(|(&id, _)| id)
            .collect();
        let mut visited = 0;

        while let Some(node) = queue.pop() {
            visited += 1;
            if let Some(neighbors) = adj.get(node) {
                for &n in neighbors {
                    if let Some(deg) = in_degree.get_mut(n) {
                        *deg -= 1;
                        if *deg == 0 {
                            queue.push(n);
                        }
                    }
                }
            }
        }

        if visited != self.workers.len() {
            return Err("cycle detected in worker DAG".into());
        }

        Ok(())
    }

    /// Compute topological levels. Workers at the same level can run in parallel.
    fn compute_levels(&self) -> Vec<Vec<usize>> {
        let id_to_idx: HashMap<&str, usize> = self
            .workers
            .iter()
            .enumerate()
            .map(|(i, w)| (w.id.as_str(), i))
            .collect();

        let mut level_of: Vec<usize> = vec![0; self.workers.len()];

        let mut changed = true;
        while changed {
            changed = false;
            for (i, worker) in self.workers.iter().enumerate() {
                for dep in &worker.dependencies {
                    if let Some(&dep_idx) = id_to_idx.get(dep.as_str()) {
                        let new_level = level_of[dep_idx] + 1;
                        if new_level > level_of[i] {
                            level_of[i] = new_level;
                            changed = true;
                        }
                    }
                }
            }
        }

        let max_level = level_of.iter().copied().max().unwrap_or(0);
        let mut levels: Vec<Vec<usize>> = vec![Vec::new(); max_level + 1];
        for (i, &level) in level_of.iter().enumerate() {
            levels[level].push(i);
        }
        levels
    }

    /// Derive a deterministic child ExecutionId from the coordinator's ID and worker name.
    fn child_id(coordinator_id: ExecutionId, worker_id: &str) -> ExecutionId {
        // Create a deterministic UUID string from the coordinator ID + worker ID
        let seed = format!("{}:{}", coordinator_id, worker_id);
        let hash1 = fnv1a_hash(seed.as_bytes());
        let hash2 = fnv1a_hash(worker_id.as_bytes());
        // Format as a valid UUID string
        let uuid_str = format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            (hash1 >> 32) as u32,
            (hash1 >> 16) as u16,
            (hash1 & 0xFFF) as u16,
            (0x8000 | (hash2 & 0x3FFF)) as u16,
            hash2 & 0xFFFFFFFFFFFF,
        );
        let uuid = Uuid::parse(&uuid_str).unwrap_or_else(|_| Uuid::new_v4());
        ExecutionId::from_uuid(uuid)
    }

    /// Execute the coordination DAG durably.
    ///
    /// On crash recovery, completed workers are skipped (their results are
    /// in the event log). Only incomplete workers re-execute.
    pub fn execute(mut self, exec_id: ExecutionId) -> DurableResult<HashMap<String, json::Value>> {
        self.validate()
            .map_err(|e| DurableError::InvalidState(e))?;

        if self.workers.is_empty() {
            return Ok(HashMap::new());
        }

        // Create or resume the coordinator's replay context
        let replay_ctx = match ReplayContext::resume(exec_id, self.event_store.clone(), None, None) {
            Ok(ctx) => ctx,
            Err(DurableError::NotFound(_)) => {
                ReplayContext::new(exec_id, self.event_store.clone(), None, None)?
            }
            Err(e) => return Err(e),
        };

        let levels = self.compute_levels();
        let results: Arc<Mutex<HashMap<String, json::Value>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Take ownership of workers into indexed slots
        let mut worker_slots: Vec<Option<WorkerDef>> =
            self.workers.drain(..).map(Some).collect();

        for level in levels {
            let mut handles = Vec::new();

            for &idx in &level {
                let worker = worker_slots[idx].take().unwrap();
                let child_id = Self::child_id(exec_id, &worker.id);

                // Check if this worker already completed (replay)
                let existing_result = replay_ctx.child_flow_result(&child_id);

                if let Some(Some(result_str)) = existing_result {
                    // Already completed — use cached result
                    if let Ok(val) = json::parse(&result_str) {
                        results.lock().unwrap().insert(worker.id, val);
                    }
                    continue;
                }

                // Gather dependency inputs
                let dep_inputs: HashMap<String, json::Value> = {
                    let res = results.lock().unwrap();
                    worker
                        .dependencies
                        .iter()
                        .filter_map(|d| res.get(d).map(|v| (d.clone(), v.clone())))
                        .collect()
                };

                let worker_id = worker.id.clone();
                let input_str = json::to_string(&json::json_str(&worker_id));

                // Record child flow start
                let _ = replay_ctx.start_child_flow(child_id, &input_str);

                let handle = std::thread::spawn(move || {
                    let result = (worker.factory)(dep_inputs);
                    (worker_id, child_id, result)
                });
                handles.push(handle);
            }

            // Wait for all workers in this level
            let mut level_errors = Vec::new();
            for handle in handles {
                match handle.join() {
                    Ok((worker_id, child_id, Ok(val))) => {
                        let result_str = json::to_string(&val);
                        let _ = replay_ctx.complete_child_flow(child_id, &result_str);
                        results.lock().unwrap().insert(worker_id, val);
                    }
                    Ok((worker_id, _child_id, Err(err))) => {
                        level_errors.push((worker_id, err));
                    }
                    Err(_) => {
                        level_errors.push((
                            "unknown".to_string(),
                            DurableError::InvalidState("worker thread panicked".into()),
                        ));
                    }
                }
            }

            if !level_errors.is_empty() {
                let (_id, first_err) = level_errors.into_iter().next().unwrap();
                let _ = replay_ctx.fail(&first_err.to_string());
                return Err(first_err);
            }
        }

        let _ = replay_ctx.complete(&json::to_string(&json::json_str("coordination_complete")));

        Ok(Arc::try_unwrap(results).unwrap().into_inner().unwrap())
    }
}
