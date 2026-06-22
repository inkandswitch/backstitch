use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Duration,
};
use tokio::time::sleep;

use crate::project::main_thread_block::MainThreadBlock;

#[tokio::test]
async fn test_no_workers() {
    let lock = Arc::new(MainThreadBlock::new());
    // Checkpoint should complete immediately with no workers
    lock.checkpoint().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn test_multiple_workers() {
    let lock = Arc::new(MainThreadBlock::new());
    let mut workers = vec![];
    let num_completed = Arc::new(AtomicU32::new(0));

    for iteration in 0..50 {
        for i in 0..5 {
            let lock_clone = Arc::clone(&lock);
            let num_completed_clone = num_completed.clone();
            workers.push(tokio::spawn(async move {
                let _guard = lock_clone.wait().await;
                sleep(Duration::from_millis(3 * i)).await;
                num_completed_clone.fetch_add(1, Ordering::Relaxed);
            }));
        }

        // Main thread checkpoint. Sleep first to ensure all workers are awaiting their guard.
        sleep(Duration::from_millis(50)).await;

        // No workers should have run yet.
        assert_eq!(num_completed.load(Ordering::Relaxed), iteration * 5);

        // Now that we're blocking in main in the correct place, run all the workers.
        lock.checkpoint().await;

        // Once we're done waiting, they should've all completed.
        assert_eq!(num_completed.load(Ordering::Relaxed), iteration * 5 + 5);
    }
}
