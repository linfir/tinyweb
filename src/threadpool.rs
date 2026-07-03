// see the Rust book's thread pool example

use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
};

type Handles = Arc<Mutex<Vec<thread::JoinHandle<()>>>>;

pub struct ThreadPool {
    sender: mpsc::SyncSender<Job>,
    handles: Handles,
}

impl ThreadPool {
    /// Creates a new ThreadPool.
    /// Panics if the size is zero.
    pub fn new(size: usize) -> Self {
        assert!(size > 0);
        let (sender, receiver) = mpsc::sync_channel(2 * size);
        let receiver = Arc::new(Mutex::new(receiver));
        let handles = Arc::new(Mutex::new(Vec::with_capacity(size)));

        for _ in 0..size {
            spawn_worker(receiver.clone(), handles.clone());
        }

        ThreadPool { sender, handles }
    }

    /// Executes a job on a thread in the pool.
    /// Returns false if the queue is full.
    pub fn execute<F>(&self, f: F) -> bool
    where
        F: FnOnce() + Send + 'static,
    {
        let job = Box::new(f);
        match self.sender.try_send(job) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Full(_)) => false,
            Err(mpsc::TrySendError::Disconnected(_)) => {
                log::error!("thread pool is gone, dropping job");
                false
            }
        }
    }

    /// Closes the job queue and joins all workers.
    /// Blocks until running jobs finish; call only when the pool is idle.
    pub fn join(self) {
        drop(self.sender);
        loop {
            let handle = lock(&self.handles).pop();
            match handle {
                Some(h) => {
                    let _ = h.join();
                }
                None => break,
            }
        }
    }
}

fn lock(handles: &Handles) -> std::sync::MutexGuard<'_, Vec<thread::JoinHandle<()>>> {
    handles.lock().unwrap_or_else(|e| e.into_inner())
}

fn spawn_worker(receiver: Arc<Mutex<mpsc::Receiver<Job>>>, handles: Handles) {
    let receiver2 = receiver.clone();
    let handles2 = handles.clone();
    let h = thread::spawn(move || {
        loop {
            let Ok(job) = receiver2.lock().unwrap_or_else(|e| e.into_inner()).recv() else {
                return;
            };
            // the lock is released here
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                log::error!("worker panicked, respawning");
                spawn_worker(receiver2, handles2);
                return;
            }
        }
    });
    lock(&handles).push(h);
}

type Job = Box<dyn FnOnce() + Send + 'static>;
