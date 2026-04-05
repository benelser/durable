//! Fixed-size thread pool with bounded work queue.
//!
//! Workers read jobs from a shared bounded channel. When the queue is full,
//! `try_submit()` returns `Err(QueueFull)` for backpressure.

use std::sync::mpsc;
use std::thread;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// A fixed-size thread pool with bounded work queue.
pub struct ThreadPool {
    workers: Vec<Worker>,
    sender: Option<mpsc::SyncSender<Job>>,
    queue_capacity: usize,
}

struct Worker {
    handle: Option<thread::JoinHandle<()>>,
}

impl ThreadPool {
    /// Create a pool with `size` worker threads and an unbounded queue (legacy).
    pub fn new(size: usize) -> Self {
        Self::with_queue_capacity(size, 1024)
    }

    /// Create a pool with `size` worker threads and a bounded work queue.
    /// When the queue reaches `queue_capacity`, `try_submit()` returns an error.
    pub fn with_queue_capacity(size: usize, queue_capacity: usize) -> Self {
        assert!(size > 0, "thread pool size must be > 0");
        assert!(queue_capacity > 0, "queue capacity must be > 0");

        let (sender, receiver) = mpsc::sync_channel::<Job>(queue_capacity);
        let receiver = std::sync::Arc::new(std::sync::Mutex::new(receiver));

        let mut workers = Vec::with_capacity(size);
        for i in 0..size {
            let rx = receiver.clone();
            let handle = thread::Builder::new()
                .name(format!("pool-worker-{}", i))
                .spawn(move || {
                    loop {
                        let job = {
                            let lock = rx.lock().unwrap_or_else(|e| e.into_inner());
                            lock.recv()
                        };
                        match job {
                            Ok(job) => job(),
                            Err(_) => break,
                        }
                    }
                })
                .expect("failed to spawn pool worker");

            workers.push(Worker {
                handle: Some(handle),
            });
        }

        Self {
            workers,
            sender: Some(sender),
            queue_capacity,
        }
    }

    /// Create a pool with a number of workers equal to available parallelism,
    /// or `fallback` if that can't be determined.
    pub fn auto(fallback: usize) -> Self {
        let size = thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(fallback);
        Self::new(size)
    }

    /// Submit a job to the pool. Blocks if the queue is full.
    /// Returns a receiver for the result.
    pub fn submit<F, T>(&self, f: F) -> mpsc::Receiver<T>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let job = Box::new(move || {
            let result = f();
            let _ = tx.send(result);
        });
        if let Some(ref sender) = self.sender {
            sender.send(job).expect("thread pool channel closed");
        }
        rx
    }

    /// Try to submit a job. Returns `true` if queued, `false` if queue is full.
    pub fn try_execute<F>(&self, f: F) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        if let Some(ref sender) = self.sender {
            match sender.try_send(Box::new(f)) {
                Ok(()) => true,
                Err(mpsc::TrySendError::Full(_)) => false,
                Err(mpsc::TrySendError::Disconnected(_)) => false,
            }
        } else {
            false
        }
    }

    /// Submit a job without waiting for the result. Blocks if queue is full.
    pub fn execute<F>(&self, f: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let job = Box::new(f);
        if let Some(ref sender) = self.sender {
            sender.send(job).expect("thread pool channel closed");
        }
    }

    /// Get the number of worker threads.
    pub fn size(&self) -> usize {
        self.workers.len()
    }

    /// Get the queue capacity.
    pub fn queue_capacity(&self) -> usize {
        self.queue_capacity
    }

    /// Shut down the pool, waiting for all workers to finish.
    pub fn shutdown(&mut self) {
        self.sender.take();
        for worker in &mut self.workers {
            if let Some(handle) = worker.handle.take() {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for ThreadPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}
