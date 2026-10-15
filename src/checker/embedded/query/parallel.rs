//! File-affine queries over the compiler's independent checker owners. Only
//! owned protocol answers cross the worker barrier; compiler handles do not.
use std::sync::Mutex;

use super::super::diagnostics::Diagnostics;
use super::*;
use crate::checker::protocol::RemoteError;

struct WorkerFailure {
    error: CheckerError,
    context: Diagnostics,
}

impl WorkerFailure {
    fn new(error: CheckerError, request: &Request) -> Self {
        Self {
            error,
            context: request.diagnostics.fork(),
        }
    }

    fn fatal(self, request: &Request) -> CheckerError {
        request.diagnostics.adopt(&self.context);
        self.error
    }
}

#[derive(Default)]
struct LaneResult {
    completed: Vec<(usize, MemberProjectResult)>,
    failure: Option<(String, WorkerFailure)>,
    panic: Option<WorkerFailure>,
}

impl BuiltProject {
    /// Complete one bounded preparation wave. A compiler error quarantines its
    /// file and stops that checker until the barrier. Healthy unfinished files
    /// resume on a fresh pool over the same immutable program.
    pub fn resolve_batch(
        &mut self,
        root: &Path,
        queries: &[MemberQuery],
        by_file: &BTreeMap<String, Vec<usize>>,
        failed_files: &mut BTreeMap<String, RemoteError>,
        request: &Arc<Request>,
    ) -> Result<Vec<MemberProjectResult>, CheckerError> {
        let project_id = self.project.summary.project_id.clone();
        let mut file_checkers = BTreeMap::new();
        for (file, indices) in by_file {
            if failed_files.contains_key(file) {
                continue;
            }
            request
                .diagnostics
                .query(&queries[indices[0]], "call-lookup");
            match self.prepare_file(root, file, request) {
                Ok(checker) => {
                    file_checkers.insert(file.as_str(), checker);
                }
                Err(failure) => {
                    request.check()?;
                    if !self.recoverable(&failure) {
                        return Err(failure);
                    }
                    // This lookup uses no checker state, so no pool recovery
                    // is needed when the source-file walk itself fails.
                    failed_files
                        .insert(file.clone(), remote(request.diagnostics.annotate(failure)));
                }
            }
        }

        // Worker errors carry their own query context. The outer request may
        // add context too, but must not name the last serially prepared file.
        request.diagnostics.project(&project_id, "query-wave");
        let mut results = vec![None; queries.len()];
        loop {
            let mut lanes = vec![Vec::new(); self.pool.checker_count()];
            for (file, indices) in by_file {
                if !failed_files.contains_key(file) && results[indices[0]].is_none() {
                    lanes[file_checkers[file.as_str()]].push((file.as_str(), indices.as_slice()));
                }
            }
            if lanes.iter().all(Vec::is_empty) {
                break;
            }
            let outcomes: Vec<_> = lanes
                .iter()
                .map(|_| Mutex::new(LaneResult::default()))
                .collect();
            #[cfg(test)]
            let fault = super::super::test_support::capture_query_fault();
            let pool_result = self.pool.for_each_checker_parallel(&|checker, operation| {
                if lanes[checker].is_empty() {
                    return;
                }
                let worker = request.worker();
                let mut run = || {
                    worker.diagnostics.project(&project_id, "checker-operation");
                    let resolve =
                        || self.resolve_lane(root, queries, &lanes[checker], &worker, operation);
                    #[cfg(test)]
                    {
                        super::super::test_support::with_optional_query_fault(fault, resolve)
                    }
                    #[cfg(not(test))]
                    {
                        let mut resolve = resolve;
                        resolve()
                    }
                };
                let run = || {
                    if self.pool.checker_count() > 1 {
                        crate::progress::without_reporting(run)
                    } else {
                        run()
                    }
                };
                let outcome = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)) {
                    Ok(outcome) => outcome,
                    Err(payload) => {
                        // The operation is borrowed from the pool callback, so
                        // catching here does not unwind its lease. Retire all
                        // owners explicitly before another query can proceed.
                        self.retire();
                        let detail = payload
                            .downcast_ref::<String>()
                            .map(String::as_str)
                            .or_else(|| payload.downcast_ref::<&str>().copied())
                            .unwrap_or("non-string panic payload");
                        LaneResult {
                            panic: Some(WorkerFailure::new(
                                error(
                                    "checker_failure",
                                    format!("embedded TSR panicked: {detail}"),
                                ),
                                &worker,
                            )),
                            ..LaneResult::default()
                        }
                    }
                };
                *outcomes[checker].lock().expect("checker lane result") = outcome;
            });
            // All workers have joined. Deadline/cancellation must retain their
            // typed errors even when a compiler call reported a generic error.
            request.check()?;
            let mut outcomes: Vec<_> = outcomes
                .into_iter()
                .map(|outcome| outcome.into_inner().expect("checker lane result"))
                .collect();
            if let Some(failure) = outcomes.iter_mut().find_map(|outcome| outcome.panic.take()) {
                return Err(failure.fatal(request));
            }
            pool_result.map_err(compiler_error)?;
            let mut recover = false;
            for outcome in outcomes {
                for (index, answer) in outcome.completed {
                    results[index] = Some(answer);
                }
                if let Some((file, failure)) = outcome.failure {
                    if !self.recoverable(&failure.error) {
                        return Err(failure.fatal(request));
                    }
                    let failure = remote(failure.context.annotate(failure.error));
                    recover |= failure.code == "checker_failure";
                    failed_files.insert(file, failure);
                }
            }
            if recover {
                self.restart_checker(request)?;
            }
        }
        Ok(queries
            .iter()
            .zip(results)
            .map(|(query, result)| match failed_files.get(&query.file) {
                Some(failure) => super::super::failed_result(&project_id, query, failure.clone()),
                None => result.expect("every query completed or its file was quarantined"),
            })
            .collect())
    }

    fn prepare_file(
        &mut self,
        root: &Path,
        name: &str,
        request: &Request,
    ) -> Result<usize, CheckerError> {
        request.check()?;
        let path = query_file(root, name)?;
        let file = self
            .program
            .source_file(path_text(&path)?.as_bytes())
            .ok_or_else(|| {
                error(
                    "project_mismatch",
                    "owned query source is absent from the compiler program",
                )
            })?;
        let checker = self.checker_indices[&file.source()];
        if !self.calls.contains_key(&path) {
            self.calls.insert(
                path.clone(),
                calls(file, &self.project.host, &path, request)?,
            );
        }
        Ok(checker)
    }

    fn resolve_lane(
        &self,
        root: &Path,
        queries: &[MemberQuery],
        files: &[(&str, &[usize])],
        request: &Request,
        operation: &mut Operation<'_>,
    ) -> LaneResult {
        let mut result = LaneResult::default();
        for &(file, indices) in files {
            for &index in indices {
                match self.resolve(root, &queries[index], request, operation) {
                    Ok(answer) => result.completed.push((index, answer)),
                    Err(failure) => {
                        result.failure = Some((file.into(), WorkerFailure::new(failure, request)));
                        return result;
                    }
                }
            }
        }
        result
    }

    fn recoverable(&self, failure: &CheckerError) -> bool {
        matches!(failure, CheckerError::Remote { code, .. }
            if code == "checker_failure"
                || self.project.summary.project_id.starts_with("inferred:")
                    && ["invalid_span", "span_mismatch"].contains(&code.as_str()))
    }
}

fn remote(failure: CheckerError) -> RemoteError {
    let CheckerError::Remote { code, message } = failure else {
        unreachable!("only remote compiler failures are quarantined")
    };
    RemoteError { code, message }
}
