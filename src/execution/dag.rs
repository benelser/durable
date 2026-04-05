//! DAG-based parallel step execution.
//!
//! When multiple steps have no data dependencies, they execute
//! concurrently via OS threads. Uses a level-based approach:
//! compute topological levels, then execute each level in parallel.

use crate::core::error::{DurableError, DurableResult};
use crate::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::thread;

/// A step registered in the DAG.
struct DagStep {
    id: String,
    dependencies: Vec<String>,
    factory: Box<dyn FnOnce(HashMap<String, json::Value>) -> DurableResult<json::Value> + Send>,
}

/// Builds and executes a DAG of steps with automatic parallelization.
pub struct DagExecutor {
    steps: Vec<DagStep>,
}

impl DagExecutor {
    pub fn new() -> Self {
        Self { steps: Vec::new() }
    }

    /// Register a step with its dependencies.
    /// The closure receives a map of dependency results keyed by step ID.
    pub fn add_step<F>(
        &mut self,
        id: impl Into<String>,
        dependencies: Vec<String>,
        f: F,
    ) where
        F: FnOnce(HashMap<String, json::Value>) -> DurableResult<json::Value> + Send + 'static,
    {
        self.steps.push(DagStep {
            id: id.into(),
            dependencies,
            factory: Box::new(f),
        });
    }

    /// Validate the DAG (check for cycles and missing dependencies).
    pub fn validate(&self) -> Result<(), String> {
        let ids: BTreeSet<&str> = self.steps.iter().map(|s| s.id.as_str()).collect();

        for step in &self.steps {
            for dep in &step.dependencies {
                if !ids.contains(dep.as_str()) {
                    return Err(format!(
                        "step '{}' depends on '{}' which doesn't exist",
                        step.id, dep
                    ));
                }
            }
        }

        // Topological sort to detect cycles
        let mut in_degree: BTreeMap<&str, usize> = BTreeMap::new();
        let mut adj: BTreeMap<&str, Vec<&str>> = BTreeMap::new();

        for step in &self.steps {
            in_degree.entry(step.id.as_str()).or_insert(0);
            adj.entry(step.id.as_str()).or_default();
            for dep in &step.dependencies {
                *in_degree.entry(step.id.as_str()).or_insert(0) += 1;
                adj.entry(dep.as_str()).or_default().push(step.id.as_str());
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

        if visited != self.steps.len() {
            return Err("cycle detected in step DAG".to_string());
        }

        Ok(())
    }

    /// Compute topological levels. Steps at the same level can run in parallel.
    fn compute_levels(&self) -> Vec<Vec<usize>> {
        let id_to_idx: HashMap<&str, usize> = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.as_str(), i))
            .collect();

        let mut level_of: Vec<usize> = vec![0; self.steps.len()];

        // BFS-like: level = max(level of dependencies) + 1
        // Simple iterative approach since DAG is small
        let mut changed = true;
        while changed {
            changed = false;
            for (i, step) in self.steps.iter().enumerate() {
                for dep in &step.dependencies {
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

    /// Execute the DAG. Steps at the same topological level run in parallel.
    pub fn execute(mut self) -> DurableResult<HashMap<String, json::Value>> {
        self.validate()
            .map_err(|e| DurableError::InvalidState(e))?;

        if self.steps.is_empty() {
            return Ok(HashMap::new());
        }

        let levels = self.compute_levels();
        let results: Arc<Mutex<HashMap<String, json::Value>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // We need to take ownership of steps. Replace with a vec of Options.
        let mut step_slots: Vec<Option<DagStep>> = self.steps.drain(..).map(Some).collect();

        for level in levels {
            let mut handles = Vec::new();

            for &idx in &level {
                let step = step_slots[idx].take().unwrap();
                let results_clone = results.clone();

                // Gather dependency inputs from prior results
                let deps = step.dependencies.clone();
                let inputs: HashMap<String, json::Value> = {
                    let res = results_clone.lock().unwrap();
                    deps.iter()
                        .filter_map(|d| res.get(d).map(|v| (d.clone(), v.clone())))
                        .collect()
                };

                let id = step.id.clone();
                let handle = thread::spawn(move || {
                    let result = (step.factory)(inputs);
                    (id, result)
                });
                handles.push(handle);
            }

            // Wait for all steps in this level to complete
            let mut level_errors = Vec::new();
            for handle in handles {
                match handle.join() {
                    Ok((id, Ok(val))) => {
                        results.lock().unwrap().insert(id, val);
                    }
                    Ok((id, Err(err))) => {
                        level_errors.push((id, err));
                    }
                    Err(_) => {
                        level_errors.push((
                            "unknown".to_string(),
                            DurableError::InvalidState("thread panicked".to_string()),
                        ));
                    }
                }
            }

            if !level_errors.is_empty() {
                let (_step_name, first_err) = level_errors.into_iter().next().unwrap();
                return Err(first_err);
            }
        }

        Ok(Arc::try_unwrap(results).unwrap().into_inner().unwrap())
    }
}

/// Simpler parallel execution: run multiple closures concurrently and collect results.
/// This is the practical API for parallel tool calls.
///
/// If a `ThreadPool` is provided, tasks are submitted to it (bounded concurrency).
/// Otherwise, raw threads are spawned (unbounded — use only for small task counts).
pub fn execute_parallel<F>(
    tasks: Vec<(String, F)>,
    pool: Option<&crate::core::pool::ThreadPool>,
) -> Vec<(String, DurableResult<json::Value>)>
where
    F: FnOnce() -> DurableResult<json::Value> + Send + 'static,
{
    match pool {
        Some(pool) => {
            let receivers: Vec<std::sync::mpsc::Receiver<_>> = tasks
                .into_iter()
                .map(|(id, f)| {
                    pool.submit(move || (id, f()))
                })
                .collect();
            receivers
                .into_iter()
                .map(|rx| {
                    rx.recv().unwrap_or_else(|_| {
                        (
                            "unknown".to_string(),
                            Err(DurableError::InvalidState("pool channel closed".to_string())),
                        )
                    })
                })
                .collect()
        }
        None => {
            // Fallback: raw threads (bounded by task count)
            let mut handles = Vec::new();
            for (id, f) in tasks {
                let handle = thread::spawn(move || (id, f()));
                handles.push(handle);
            }
            handles
                .into_iter()
                .map(|h| {
                    h.join().unwrap_or_else(|_| {
                        (
                            "unknown".to_string(),
                            Err(DurableError::InvalidState(
                                "thread panicked".to_string(),
                            )),
                        )
                    })
                })
                .collect()
        }
    }
}

impl Default for DagExecutor {
    fn default() -> Self {
        Self::new()
    }
}
