//! Background worker for automatic resumption of suspended executions.
//!
//! The worker runs a polling loop that:
//! 1. Checks for expired timers and fires them as signals
//! 2. Checks for suspended executions with pending signals and resumes them
//! 3. Respects backpressure limits

use crate::agent::runtime::AgentRuntime;
use crate::core::cancel::CancellationToken;
use crate::core::error::SuspendReason;
use crate::core::pool::ThreadPool;
use crate::core::types::*;
use crate::storage::ExecutionLog;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// Worker configuration.
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    /// How often to poll for work (default: 1 second).
    pub poll_interval: Duration,
    /// Maximum concurrent executions (backpressure).
    pub max_concurrent: usize,
    /// Graceful shutdown timeout.
    pub shutdown_timeout: Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            max_concurrent: 4,
            shutdown_timeout: Duration::from_secs(30),
        }
    }
}

/// Handle to a running worker. Used for shutdown.
pub struct WorkerHandle {
    thread: Option<thread::JoinHandle<()>>,
    cancel: CancellationToken,
    _shutdown_timeout: Duration,
}

impl WorkerHandle {
    /// Signal the worker to stop and wait for it to exit.
    pub fn shutdown(mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }

    /// Check if the worker is still running.
    pub fn is_running(&self) -> bool {
        self.thread
            .as_ref()
            .map(|h| !h.is_finished())
            .unwrap_or(false)
    }

    /// Get the cancellation token (for external cancellation).
    pub fn cancel_token(&self) -> &CancellationToken {
        &self.cancel
    }
}

impl Drop for WorkerHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.thread.take() {
            // Best-effort join within timeout
            let _ = handle.join();
        }
    }
}

/// Background worker that automatically resumes suspended executions.
pub struct Worker {
    config: WorkerConfig,
    runtime: Arc<AgentRuntime>,
    storage: Arc<dyn ExecutionLog>,
    pool: Arc<ThreadPool>,
}

impl Worker {
    pub fn new(
        config: WorkerConfig,
        runtime: Arc<AgentRuntime>,
        storage: Arc<dyn ExecutionLog>,
    ) -> Self {
        let pool = Arc::new(ThreadPool::new(config.max_concurrent));
        Self {
            config,
            runtime,
            storage,
            pool,
        }
    }

    /// Start the worker in a background thread.
    pub fn start(self) -> WorkerHandle {
        let cancel = CancellationToken::new();
        let cancel_clone = cancel.clone();
        let shutdown_timeout = self.config.shutdown_timeout;

        let handle = thread::Builder::new()
            .name("durable-worker".to_string())
            .spawn(move || {
                self.run_loop(&cancel_clone);
            })
            .expect("failed to spawn worker thread");

        WorkerHandle {
            thread: Some(handle),
            cancel,
            _shutdown_timeout: shutdown_timeout,
        }
    }

    fn run_loop(&self, cancel: &CancellationToken) {
        let active = Arc::new(AtomicUsize::new(0));

        while !cancel.is_cancelled() {
            // Backpressure: check if we're at capacity
            if active.load(Ordering::SeqCst) >= self.config.max_concurrent {
                thread::sleep(self.config.poll_interval);
                continue;
            }

            // 1. Check for expired timers
            if let Ok(expired) = self.storage.get_expired_timers() {
                for (exec_id, timer_name, _fire_at) in expired {
                    if cancel.is_cancelled() {
                        return;
                    }
                    // Fire the timer as a signal
                    let _ = self.storage.delete_timer(exec_id, &timer_name);
                    let _ = self.storage.store_signal(
                        exec_id,
                        &timer_name,
                        "true",
                    );
                }
            }

            // 2. Check suspended executions for ready signals
            if let Ok(suspended) = self
                .storage
                .list_executions(Some(ExecutionStatus::Suspended))
            {
                for meta in suspended {
                    if cancel.is_cancelled() {
                        return;
                    }
                    // Atomic admission: increment first, check, rollback if over limit
                    let prev = active.fetch_add(1, Ordering::SeqCst);
                    if prev >= self.config.max_concurrent {
                        active.fetch_sub(1, Ordering::SeqCst);
                        break;
                    }

                    let should_resume = match &meta.suspend_reason {
                        Some(SuspendReason::WaitingForSignal { signal_name }) => {
                            self.storage
                                .peek_signal(meta.id, signal_name)
                                .ok()
                                .flatten()
                                .is_some()
                        }
                        Some(SuspendReason::WaitingForConfirmation {
                            confirmation_id, ..
                        }) => {
                            self.storage
                                .peek_signal(meta.id, confirmation_id)
                                .ok()
                                .flatten()
                                .is_some()
                        }
                        Some(SuspendReason::WaitingForInput { .. }) => {
                            self.storage
                                .peek_signal(meta.id, "__user_input")
                                .ok()
                                .flatten()
                                .is_some()
                        }
                        Some(SuspendReason::WaitingForTimer { timer_name, .. }) => {
                            self.storage
                                .peek_signal(meta.id, timer_name)
                                .ok()
                                .flatten()
                                .is_some()
                        }
                        Some(SuspendReason::WaitingForChild { child_id }) => {
                            // Check if child completed by looking for its signal
                            let signal_name = format!("__child_{}", child_id);
                            self.storage
                                .peek_signal(meta.id, &signal_name)
                                .ok()
                                .flatten()
                                .is_some()
                        }
                        // Contract violations and budget exhaustion require
                        // explicit human intervention — don't auto-resume.
                        Some(SuspendReason::ContractViolation { .. }) => false,
                        Some(SuspendReason::BudgetExhausted { .. }) => false,
                        Some(SuspendReason::GracefulShutdown) => false,
                        None => false,
                    };

                    if should_resume {
                        let runtime = self.runtime.clone();
                        let active_clone = active.clone();
                        let exec_id = meta.id;
                        // Slot already reserved by fetch_add above

                        if !self.pool.try_execute(move || {
                            let _ = runtime.resume(exec_id);
                            active_clone.fetch_sub(1, Ordering::SeqCst);
                        }) {
                            // Queue full — release the slot
                            active.fetch_sub(1, Ordering::SeqCst);
                        }
                    } else {
                        // Not resuming — release the slot
                        active.fetch_sub(1, Ordering::SeqCst);
                    }
                }
            }

            // Sleep until next poll
            // Use small increments to check cancellation
            let mut slept = Duration::ZERO;
            let increment = Duration::from_millis(100);
            while slept < self.config.poll_interval && !cancel.is_cancelled() {
                thread::sleep(increment);
                slept += increment;
            }
        }
    }
}
