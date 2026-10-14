//! Bounded project execution with coordinator-owned result handling.
use anyhow::{Result, anyhow};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc,
};

pub(super) fn run<T: Sync, R: Send>(
    jobs: &[T],
    limit: usize,
    canceled: impl Fn() -> bool + Sync,
    work: impl Fn(&T) -> R + Sync,
    mut complete: impl FnMut(&T, R) -> Result<()>,
) -> Result<()> {
    super::validate_jobs(limit)?;
    if limit == 1 || jobs.len() <= 1 {
        for job in jobs {
            if canceled() {
                break;
            }
            complete(job, work(job))?;
        }
        return Ok(());
    }
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::channel();
        for _ in 0..limit.min(jobs.len()) {
            let sender = sender.clone();
            let (next, stopped, canceled, work) = (&next, &stopped, &canceled, &work);
            let worker = std::thread::Builder::new()
                .name("jscout-checker-project".into())
                .stack_size(8 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    crate::progress::without_reporting(|| {
                        while !stopped.load(Ordering::SeqCst) && !canceled() {
                            let index = next.fetch_add(1, Ordering::SeqCst);
                            let Some(job) = jobs.get(index) else {
                                break;
                            };
                            let result =
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    work(job)
                                }));
                            if result.is_err() {
                                stopped.store(true, Ordering::SeqCst);
                            }
                            if sender.send((index, result)).is_err() {
                                break;
                            }
                        }
                    });
                });
            if let Err(error) = worker {
                stopped.store(true, Ordering::SeqCst);
                return Err(error.into());
            }
        }
        drop(sender);
        let mut outcome = Ok(());
        for (index, result) in receiver {
            if outcome.is_ok() {
                outcome = match result {
                    Ok(result) => complete(&jobs[index], result),
                    Err(_) => Err(anyhow!(
                        "checker project worker panicked; staged work retained"
                    )),
                };
                if outcome.is_err() {
                    stopped.store(true, Ordering::SeqCst);
                }
            }
        }
        outcome
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn workers_overlap_without_exceeding_limit_and_deliver_each_result() -> Result<()> {
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        let started = AtomicUsize::new(0);
        let mut completed = Vec::new();
        run(
            &[0, 1, 2, 3, 4, 5],
            2,
            || false,
            |job| {
                let count = active.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(count, Ordering::SeqCst);
                started.fetch_add(1, Ordering::SeqCst);
                let deadline = Instant::now() + Duration::from_secs(5);
                while started.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                    std::thread::yield_now();
                }
                assert!(started.load(Ordering::SeqCst) >= 2, "projects ran serially");
                active.fetch_sub(1, Ordering::SeqCst);
                job * 2
            },
            |job, answer| {
                completed.push((*job, answer));
                Ok(())
            },
        )?;
        completed.sort_unstable();
        assert_eq!(
            completed,
            vec![(0, 0), (1, 2), (2, 4), (3, 6), (4, 8), (5, 10)]
        );
        assert_eq!(peak.load(Ordering::SeqCst), 2);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        Ok(())
    }

    #[test]
    fn cancellation_stops_queued_projects_and_joins_active_workers() -> Result<()> {
        let canceled = AtomicBool::new(false);
        let started = AtomicUsize::new(0);
        let mut completed = 0;
        run(
            &[0; 12],
            2,
            || canceled.load(Ordering::SeqCst),
            |_| {
                started.fetch_add(1, Ordering::SeqCst);
                canceled.store(true, Ordering::SeqCst);
            },
            |_, ()| {
                completed += 1;
                Ok(())
            },
        )?;
        assert!((1..=2).contains(&completed));
        assert_eq!(started.load(Ordering::SeqCst), completed);
        Ok(())
    }

    #[test]
    fn panic_and_coordinator_failure_are_errors_not_successful_completion() {
        let panic = run(
            &[0, 1, 2],
            2,
            || false,
            |_| panic!("worker failure"),
            |_, ()| Ok(()),
        );
        assert!(panic.unwrap_err().to_string().contains("worker panicked"));
        let failed = run(
            &[0, 1, 2],
            2,
            || false,
            |_| (),
            |_, ()| Err(anyhow!("staging failure")),
        );
        assert!(failed.unwrap_err().to_string().contains("staging failure"));
    }
}
