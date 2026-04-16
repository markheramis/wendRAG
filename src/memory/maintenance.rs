/**
 * Background maintenance task for the memory subsystem.
 *
 * Periodically runs decay, pruning, and session cleanup on a configurable
 * interval. Shuts down gracefully via tokio cancellation.
 */

use std::sync::Arc;
use std::time::Duration;

use crate::memory::manager::MemoryManager;

/**
 * Spawns a background task that runs memory maintenance on a fixed interval.
 * The task stops when the returned JoinHandle is aborted or the runtime shuts down.
 */
pub fn spawn_maintenance_task(
    manager: Arc<MemoryManager>,
    interval: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.tick().await;

        loop {
            tick.tick().await;
            match manager.run_maintenance().await {
                Ok(result) => {
                    tracing::debug!(
                        sessions_cleaned = result.expired_sessions_cleaned,
                        entries_pruned = result.entries_pruned,
                        entries_decayed = result.entries_decayed,
                        "memory maintenance cycle complete"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "memory maintenance cycle failed");
                }
            }
        }
    })
}
