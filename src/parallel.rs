//! Order-preserving parallel map over a slice using scoped threads.

use std::cell::Cell;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;

/// Worker count for CPU-bound image work. Capped because every worker holds a
/// full-resolution decoded image.
pub fn cpu_jobs() -> usize {
    thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(8)
}

thread_local! {
    /// Set on worker threads: a map started inside a worker runs inline, so
    /// nested parallel stages (a CBZ inside a parallel merge) cannot multiply
    /// the thread count and the decoded images held at once.
    static IN_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Lock `mutex`. A thread that panicked while holding it cannot have left
/// bpdf's simple state half-updated, so a poisoned lock is used as is.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Apply `work` to every item with at most `jobs` threads; results keep the
/// input order.
pub fn map<'a, T, R, F>(items: &'a [T], jobs: usize, work: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&'a T) -> R + Sync,
{
    if items.len() <= 1 || jobs <= 1 || IN_WORKER.get() {
        return items.iter().map(work).collect();
    }

    let next = AtomicUsize::new(0);
    let results = Mutex::new(
        std::iter::repeat_with(|| None)
            .take(items.len())
            .collect::<Vec<Option<R>>>(),
    );
    thread::scope(|scope| {
        for _ in 0..jobs.min(items.len()) {
            scope.spawn(|| {
                IN_WORKER.set(true);
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = work(item);
                    lock(&results)[index] = Some(result);
                }
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|error| error.into_inner())
        .into_iter()
        .map(|result| result.expect("each parallel item is processed"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn preserves_input_order() {
        assert_eq!(
            super::map(&[3, 1, 2, 4], 3, |value| value * 2),
            vec![6, 2, 4, 8]
        );
    }

    #[test]
    fn nested_maps_run_inline_on_workers() {
        let outer = super::map(&[0, 1, 2, 3], 4, |_| {
            let id = std::thread::current().id();
            super::map(&[0, 1, 2], 3, |_| std::thread::current().id() == id)
        });
        assert!(outer.into_iter().flatten().all(|same| same));
    }
}
