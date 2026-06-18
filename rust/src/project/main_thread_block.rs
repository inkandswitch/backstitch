use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};
use tokio::sync::Semaphore;

#[cfg(test)]
mod test;

/// A lock that blocks a main thread with checkpoint() until all worker threads have dropped their guards from wait().
/// Worker threads also cannot begin execution until main has reached a checkpoint.
/// This has the effect of blocking the main thread until all subscribed worker threads have completed.
#[derive(Clone, Debug)]
pub struct MainThreadBlock {
    start: Arc<Semaphore>,
    done: Arc<Semaphore>,
    registered: Arc<AtomicU32>,
}

impl MainThreadBlock {
    pub fn new() -> Self {
        Self {
            // Allows us to let N threads through the gate, based on however many have registered.
            start: Arc::new(Semaphore::new(0)),
            // The main thread acquires N permits from done, based on how many threads it's expecting to do work.
            done: Arc::new(Semaphore::new(0)),
            // Tracks how many threads want through the gate this checkpoint.
            registered: Arc::new(AtomicU32::new(0)),
        }
    }

    /// Called by worker threads. Acquires a Guard that forces a checkpoint to begin before it can run.
    /// Checkpoint will block until all locked tasks are complete.
    pub async fn wait(&self) -> MainThreadGuard {
        // Register for this round.
        // Once a round is started, we can't get into the main thread and will have to wait til next frame.
        self.registered.fetch_add(1, Ordering::AcqRel);

        // Wait until main opens the gate
        let permit = self.start.clone().acquire_owned().await.unwrap();

        MainThreadGuard {
            lock: self.clone(),
            permit: Some(permit),
        }
    }

    /// Called by the main thread. If there are waiting threads asking to block the main thread with lock(),
    /// releases work permits to the tasks, and does not resolve until those tasks have been completed.
    /// This MUST be called by only one thread at a time!!!!!!!!!
    pub async fn checkpoint(&self) {
        // Snapshot how many workers registered before the cutoff
        let n = self.registered.swap(0, Ordering::AcqRel);

        // Open the gate for exactly those workers
        self.start.add_permits(n as usize);

        // Wait until all of them finish. We decrease the number of available permits by n each time via forgetting
        // in order to ensure we hit 0 permits at the end, so we don't accidentally allow done() through prematurely
        // next time.
        self.done.acquire_many(n.into()).await.unwrap().forget();
    }
}

pub struct MainThreadGuard {
    lock: MainThreadBlock,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}

impl Drop for MainThreadGuard {
    fn drop(&mut self) {
        // We must forget the permit, to decrease the permit capacity back to 0.
        // This way, when a checkpoint hits, we can still add N permits.
        let permit = self.permit.take();
        permit.unwrap().forget();
        // Free the main thread by 1 permit
        self.lock.done.add_permits(1);
    }
}
