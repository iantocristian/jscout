//! The worker count is execution policy, not a change to checker answers.
use std::fmt::Write;
use std::fs;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::Duration;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(60);

fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for (name, text) in files {
        let path = directory.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    directory
}

fn query(source: &str, file: &str, call: &str, receiver: &str, property: &str) -> MemberQuery {
    let start = source.rfind(call).unwrap();
    let receiver_start = start + call.find(receiver).unwrap();
    let property_start = start + call.find(property).unwrap();
    MemberQuery {
        file: file.into(),
        indexed_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
        call_start: start as i64,
        call_end: (start + call.len()) as i64,
        receiver_start: receiver_start as i64,
        receiver_end: (receiver_start + receiver.len()) as i64,
        property_start: property_start as i64,
        property_end: (property_start + property.len()) as i64,
    }
}

fn independent_files(prefix: &str, count: usize) -> (tempfile::TempDir, Vec<MemberQuery>) {
    let directory = fixture(&[(
        "tsconfig.json",
        r#"{"compilerOptions":{"noLib":true},"include":["*.ts"]}"#,
    )]);
    let mut queries = Vec::new();
    for index in 0..count {
        let file = format!("{prefix}-{index:02}.ts");
        let source = format!(
            "export interface Service{index} {{ ping(): void }} \
             declare const receiver: Service{index}; receiver.ping();"
        );
        fs::write(directory.path().join(&file), &source).unwrap();
        queries.push(query(&source, &file, "receiver.ping()", "receiver", "ping"));
    }
    (directory, queries)
}

fn checker(root: &Path, count: usize, diagnostics: bool) -> EmbeddedChecker {
    let mut checker = EmbeddedChecker::new(root, diagnostics).unwrap();
    checker.configure_checkers(count).unwrap();
    checker
}

#[test]
fn worker_counts_preserve_answers_observations_and_fingerprints() {
    let directory = fixture(&[
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"strict":true,"target":"ES2022","paths":{"@api":["./barrel.ts"]}},"include":["*.ts"]}"#,
        ),
        ("service.ts", "export interface Service { ping(): string }"),
        (
            "barrel.ts",
            "export { Service as PublicService } from './service';",
        ),
        (
            "missing.ts",
            "import { receiver } from './does-not-exist'; receiver.ping();",
        ),
    ]);
    let mut queries = Vec::new();
    for index in 0..12 {
        let file = format!("consumer-{index:02}.ts");
        let source = "import { PublicService as Alias } from '@api'; \
                      declare const receiver: Alias; receiver.ping(); \
                      const values: number[] = []; values.map(value => value + 1);";
        fs::write(directory.path().join(&file), source).unwrap();
        queries.push(query(source, &file, "receiver.ping()", "receiver", "ping"));
        queries.push(query(
            source,
            &file,
            "values.map(value => value + 1)",
            "values",
            "map",
        ));
    }
    queries.push(query(
        "import { receiver } from './does-not-exist'; receiver.ping();",
        "missing.ts",
        "receiver.ping()",
        "receiver",
        "ping",
    ));
    let mut reference = None;
    // Repeat the four-worker arm to expose schedule-dependent observations.
    for count in [1, 2, 4, 4] {
        let mut checker = checker(directory.path(), count, false);
        assert!(
            checker
                .prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
                .unwrap()
                .is_empty()
        );
        let built = checker.built.as_ref().unwrap();
        let inputs = built
            .inputs
            .iter()
            .map(|input| (input.path.clone(), input.source_hash.clone()))
            .collect::<Vec<_>>();
        let fingerprint = built.fingerprint.clone();
        let result = checker
            .resolve_members("tsconfig.json".into(), vec![], queries.clone(), TIMEOUT)
            .unwrap();
        assert_eq!(result.checker_input_fingerprint, fingerprint);
        assert_eq!(result.results.last().unwrap().answer.status, "unknown");
        for answers in result.results[..24].chunks_exact(2) {
            assert_eq!(answers[0].answer.status, "resolved");
            assert_eq!(answers[0].answer.declarations.len(), 1);
            assert_eq!(
                answers[0].answer.declarations[0].file.as_deref(),
                Some("service.ts")
            );
            assert_eq!(answers[1].answer.status, "resolved");
            assert_eq!(
                answers[1].answer.declarations[0].context.as_deref(),
                Some("lib")
            );
        }
        let actual = (
            serde_json::to_value(&result.results).unwrap(),
            inputs,
            fingerprint,
        );
        if let Some(expected) = &reference {
            assert_eq!(&actual, expected, "checker count {count}");
        } else {
            reference = Some(actual);
        }
        assert!(
            checker
                .validate_project(
                    "tsconfig.json".into(),
                    result.checker_input_fingerprint,
                    TIMEOUT,
                )
                .unwrap()
                .valid
        );
    }
}

#[test]
fn independent_files_execute_on_concurrent_query_workers() {
    // A timed rendezvous makes serial execution fail without hanging the suite.
    // Only this test installs this callback; propagated thread-local hooks do
    // not intercept unrelated tests running in the same process.
    static STARTED: AtomicUsize = AtomicUsize::new(0);
    static GATE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
    fn rendezvous(query: &MemberQuery) -> Result<(), CheckerError> {
        assert!(query.file.starts_with("concurrent-query-"));
        let (mutex, changed) = GATE.get_or_init(|| (Mutex::new(false), Condvar::new()));
        let ordinal = STARTED.fetch_add(1, Ordering::SeqCst);
        if ordinal < 2 {
            let mut ready = mutex.lock().unwrap();
            if ordinal == 1 {
                *ready = true;
                changed.notify_all();
            } else {
                let (ready, _) = changed
                    .wait_timeout_while(ready, Duration::from_secs(5), |ready| !*ready)
                    .unwrap();
                assert!(*ready, "a second checker must run while the first is busy");
            }
        }
        Ok(())
    }
    let (directory, queries) = independent_files("concurrent-query", 16);
    let mut checker = checker(directory.path(), 4, false);
    test_support::with_query_fault(rendezvous, || {
        checker
            .prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
            .unwrap();
    });
    assert_eq!(STARTED.load(Ordering::SeqCst), queries.len());
    assert!(
        checker
            .prepared
            .as_ref()
            .unwrap()
            .values()
            .all(|result| result.answer.status == "resolved")
    );
}

#[test]
fn parallel_failure_quarantines_earlier_batches_and_preserves_healthy_files() {
    static FAILURE_START: AtomicI64 = AtomicI64::new(-1);
    static FAILED_CALLS: AtomicUsize = AtomicUsize::new(0);
    fn fault(query: &MemberQuery) -> Result<(), CheckerError> {
        if query.file == "quarantine-bad.ts"
            && query.call_start == FAILURE_START.load(Ordering::SeqCst)
        {
            FAILED_CALLS.fetch_add(1, Ordering::SeqCst);
            return Err(error("checker_failure", "parallel quarantine probe"));
        }
        Ok(())
    }
    let (directory, mut queries) = independent_files("quarantine-good", 16);
    // Keep healthy files on both sides of the failure's preparation batch.
    let later_healthy = queries.split_off(8);
    let mut source = String::from(
        "export interface Bad { ping(n: number): void } declare const receiver: Bad;\n",
    );
    for index in 0..260 {
        writeln!(source, "receiver.ping({index});").unwrap();
    }
    fs::write(directory.path().join("quarantine-bad.ts"), &source).unwrap();
    let bad_queries = (0..260)
        .map(|index| {
            query(
                &source,
                "quarantine-bad.ts",
                &format!("receiver.ping({index})"),
                "receiver",
                "ping",
            )
        })
        .collect::<Vec<_>>();
    let failure = bad_queries[128].clone();
    FAILURE_START.store(failure.call_start, Ordering::SeqCst);
    queries.extend(bad_queries);
    queries.extend(later_healthy);
    let mut checker = checker(directory.path(), 4, true);
    let failed = test_support::with_query_fault(fault, || {
        checker.prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
    })
    .unwrap();
    assert_eq!(failed, BTreeSet::from(["quarantine-bad.ts".into()]));
    assert_eq!(FAILED_CALLS.load(Ordering::SeqCst), 1);
    for batch in queries.chunks(100) {
        let replay = checker
            .resolve_members("tsconfig.json".into(), vec![], batch.to_vec(), TIMEOUT)
            .unwrap();
        for (query, result) in batch.iter().zip(replay.results) {
            assert_eq!(
                result.answer.checker_input_fingerprint,
                replay.checker_input_fingerprint
            );
            if query.file == "quarantine-bad.ts" {
                assert_eq!(result.answer.status, "failed");
                assert!(result.answer.declarations.is_empty());
                assert!(result.answer.receiver_type.is_none());
                let error = result.answer.error.unwrap();
                assert!(error.message.contains("file=\"quarantine-bad.ts\""));
                assert!(error.message.contains(&format!(
                    "call_bytes={}..{}",
                    failure.call_start, failure.call_end
                )));
                assert!(error.message.contains("operation=receiver-type"));
                assert!(!error.message.contains("quarantine-good"));
            } else {
                assert_eq!(result.answer.status, "resolved");
                assert_eq!(result.answer.declarations.len(), 1);
                assert!(result.answer.error.is_none());
            }
        }
    }
    let fingerprint = checker.built.as_ref().unwrap().fingerprint.clone();
    assert!(
        checker
            .validate_project("tsconfig.json".into(), fingerprint, TIMEOUT)
            .unwrap()
            .valid
    );
}

#[test]
fn overlapping_failures_keep_their_own_query_diagnostic_context() {
    static STARTED: AtomicUsize = AtomicUsize::new(0);
    static GATE: OnceLock<(Mutex<bool>, Condvar)> = OnceLock::new();
    fn fault(query: &MemberQuery) -> Result<(), CheckerError> {
        let ordinal = STARTED.fetch_add(1, Ordering::SeqCst);
        let (mutex, changed) = GATE.get_or_init(|| (Mutex::new(false), Condvar::new()));
        if ordinal < 2 {
            let mut ready = mutex.lock().unwrap();
            if ordinal == 1 {
                *ready = true;
                changed.notify_all();
            } else {
                let (ready, _) = changed
                    .wait_timeout_while(ready, Duration::from_secs(5), |ready| !*ready)
                    .unwrap();
                assert!(*ready, "faults must overlap on different checker workers");
            }
        }
        Err(error("checker_failure", format!("probe: {}", query.file)))
    }
    let (directory, queries) = independent_files("diagnostic-worker", 16);
    let mut checker = checker(directory.path(), 4, true);
    let failed = test_support::with_query_fault(fault, || {
        checker.prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
    })
    .unwrap();
    assert_eq!(failed.len(), queries.len());
    let replay = checker
        .resolve_members("tsconfig.json".into(), vec![], queries.clone(), TIMEOUT)
        .unwrap();
    for (query, result) in queries.iter().zip(replay.results) {
        assert_eq!(result.answer.status, "failed");
        let failure = result.answer.error.unwrap();
        assert!(failure.message.contains(&format!("file={:?}", query.file)));
        assert!(failure.message.contains(&format!(
            "call_bytes={}..{}",
            query.call_start, query.call_end
        )));
        assert!(failure.message.contains("operation=receiver-type"));
        assert!(failure.message.contains(&format!("probe: {}", query.file)));
    }
}

#[test]
fn parallel_panic_discards_the_pool_and_a_new_request_succeeds() {
    let (directory, queries) = independent_files("panic-worker", 16);
    let mut checker = checker(directory.path(), 4, true);
    let failure = test_support::with_query_fault(
        |query| {
            assert_ne!(query.file, "panic-worker-00.ts", "parallel query panic");
            Ok(())
        },
        || checker.prepare_project("tsconfig.json", &[], &queries, TIMEOUT),
    )
    .unwrap_err();
    assert!(
        matches!(&failure, CheckerError::Remote { code, message }
            if code == "checker_failure"
            && message.contains("parallel query panic")
            && message.contains("file=\"panic-worker-00.ts\"")
            && message.contains("operation=receiver-type")),
        "{failure}"
    );
    assert!(checker.built.is_none());
    assert!(checker.prepared.is_none());
    assert!(checker.failed_files.is_empty());
    assert!(
        checker
            .prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
            .unwrap()
            .is_empty()
    );
    assert!(
        checker
            .prepared
            .as_ref()
            .unwrap()
            .values()
            .all(|result| result.answer.status == "resolved")
    );
}

#[test]
fn checker_count_rejects_invalid_and_late_changes() {
    let (directory, queries) = independent_files("count-validation", 4);
    let mut checker = EmbeddedChecker::new(directory.path(), false).unwrap();
    for count in [0, 17, usize::MAX] {
        assert!(checker.configure_checkers(count).is_err(), "count {count}");
    }
    checker.configure_checkers(2).unwrap();
    checker
        .prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
        .unwrap();
    assert!(checker.configure_checkers(4).is_err());
    let replay = checker
        .resolve_members("tsconfig.json".into(), vec![], queries, TIMEOUT)
        .unwrap();
    assert!(
        replay
            .results
            .iter()
            .all(|result| result.answer.status == "resolved")
    );
}

#[test]
fn expired_parallel_request_discards_all_prepared_state() {
    let (directory, queries) = independent_files("timeout-worker", 8);
    let mut checker = checker(directory.path(), 4, false);
    checker
        .prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
        .unwrap();
    assert!(matches!(
        checker.resolve_members(
            "tsconfig.json".into(),
            vec![],
            queries.clone(),
            Duration::ZERO,
        ),
        Err(CheckerError::Timeout(_))
    ));
    assert!(checker.built.is_none());
    assert!(checker.prepared.is_none());
    assert!(checker.failed_files.is_empty());
    assert!(
        checker
            .prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn deadline_during_parallel_queries_joins_workers_and_keeps_typed_timeout() {
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    static FINISHED: AtomicBool = AtomicBool::new(false);
    fn delayed_failure(query: &MemberQuery) -> Result<(), CheckerError> {
        struct Active;
        impl Drop for Active {
            fn drop(&mut self) {
                ACTIVE.fetch_sub(1, Ordering::SeqCst);
            }
        }
        ACTIVE.fetch_add(1, Ordering::SeqCst);
        let _active = Active;
        if query.file == "mid-deadline-00.ts" {
            // This uninterruptible compiler operation exceeds the request's
            // cooperative deadline, then returns a generic compiler failure.
            std::thread::sleep(Duration::from_millis(1_200));
            FINISHED.store(true, Ordering::SeqCst);
            return Err(error("checker_failure", "failure after request deadline"));
        }
        Ok(())
    }
    let (directory, queries) = independent_files("mid-deadline", 16);
    let mut checker = checker(directory.path(), 4, false);
    // Warm discovery, loading, checker initialization, and call lookup outside
    // the short deadline, leaving only the actual query workers under test.
    checker
        .resolve_members("tsconfig.json".into(), vec![], queries.clone(), TIMEOUT)
        .unwrap();
    let failure = test_support::with_query_fault(delayed_failure, || {
        checker.resolve_members(
            "tsconfig.json".into(),
            vec![],
            queries.clone(),
            Duration::from_secs(1),
        )
    })
    .unwrap_err();
    assert!(matches!(failure, CheckerError::Timeout(_)), "{failure}");
    assert!(
        FINISHED.load(Ordering::SeqCst),
        "the slow worker must finish"
    );
    assert_eq!(
        ACTIVE.load(Ordering::SeqCst),
        0,
        "all workers must be joined"
    );
    assert!(checker.built.is_none());
    assert!(checker.prepared.is_none());
    assert!(checker.failed_files.is_empty());
    let result = checker
        .resolve_members("tsconfig.json".into(), vec![], queries, TIMEOUT)
        .unwrap();
    assert!(
        result
            .results
            .iter()
            .all(|result| result.answer.status == "resolved")
    );
}
