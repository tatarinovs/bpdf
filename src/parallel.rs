//! Order-preserving parallel map over a slice using scoped threads.

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Worker count for CPU-bound image work. Capped because every worker holds a
/// full-resolution decoded image.
pub fn cpu_jobs() -> usize {
    thread::available_parallelism()
        .map_or(1, NonZeroUsize::get)
        .min(8)
}

/// Apply `work` to every item with at most `jobs` threads; results keep the
/// input order.
pub fn map<'a, T, R, F>(items: &'a [T], jobs: usize, work: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&'a T) -> R + Sync,
{
    if items.len() <= 1 || jobs <= 1 {
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
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = work(item);
                    results.lock().unwrap_or_else(|error| error.into_inner())[index] = Some(result);
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
}
