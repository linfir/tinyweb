// see the Rust book's thread pool example

use std::{
    sync::{Arc, Mutex, mpsc},
    thread,
};

pub struct ThreadPool {
    sender: mpsc::SyncSender<Job>,
}

impl ThreadPool {
    /// Creates a new ThreadPool.
    /// Panics if the size is zero.
    pub fn new(size: usize) -> Self {
        assert!(size > 0);
        let (sender, receiver) = mpsc::sync_channel(2 * size);
        let receiver = Arc::new(Mutex::new(receiver));

        for _ in 0..size {
            spawn_worker(receiver.clone());
        }

        ThreadPool { sender }
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
}

fn spawn_worker(receiver: Arc<Mutex<mpsc::Receiver<Job>>>) {
    thread::spawn(move || {
        loop {
            let job = receiver.lock().unwrap().recv().unwrap();
            // the lock is released here
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(job)).is_err() {
                log::error!("worker panicked, respawning");
                spawn_worker(receiver);
                return;
            }
        }
    });
}

type Job = Box<dyn FnOnce() + Send + 'static>;
