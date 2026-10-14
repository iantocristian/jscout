//! Request-local context survives compiler unwinding without holding a lock
//! across compiler calls. Error codes remain unchanged for retry decisions.
use std::fmt::Write;
use std::sync::Mutex;

use crate::checker::process::CheckerError;
use crate::checker::protocol::MemberQuery;

#[derive(Default)]
struct Context {
    project: Option<String>,
    query: Option<(String, i64, i64)>,
    operation: &'static str,
}

impl Context {
    fn label(&self) -> String {
        let mut text = String::from("embedded TSR");
        if let Some(project) = &self.project {
            let _ = write!(text, " project={project:?}");
        }
        if let Some((file, start, end)) = &self.query {
            let _ = write!(text, " file={file:?} call_bytes={start}..{end}");
        }
        let _ = write!(text, " operation={}", self.operation);
        text
    }
}

pub(super) struct Diagnostics(Option<Mutex<Context>>);

impl Diagnostics {
    pub fn new(enabled: bool) -> Self {
        Self(enabled.then(|| Mutex::new(Context::default())))
    }

    fn update(&self, change: impl FnOnce(&mut Context)) {
        // Keep path cloning, locking, formatting, and progress behind the opt-in.
        let Some(context) = &self.0 else { return };
        let mut context = context.lock().expect("checker diagnostic context");
        change(&mut context);
        // The reporter throttles details to its heartbeat. Parallel workers
        // suppress progress, but keep their own error context above.
        crate::progress::detail_with(|| context.label());
    }

    pub fn project(&self, project: &str, operation: &'static str) {
        self.update(|context| {
            *context = Context {
                project: Some(project.into()),
                query: None,
                operation,
            };
        });
    }

    pub fn query(&self, query: &MemberQuery, operation: &'static str) {
        self.update(|context| {
            context.query = Some((query.file.clone(), query.call_start, query.call_end));
            context.operation = operation;
        });
    }

    pub fn operation(&self, operation: &'static str) {
        self.update(|context| context.operation = operation);
    }

    pub fn annotate(&self, error: CheckerError) -> CheckerError {
        let Some(context) = &self.0 else { return error };
        let context = context.lock().expect("checker diagnostic context");
        if context.operation.is_empty() {
            return error;
        }
        let label = context.label();
        match error {
            CheckerError::Remote { code, message } => CheckerError::Remote {
                code,
                message: format!("{label}: {message}"),
            },
            CheckerError::Canceled(message) => {
                CheckerError::Canceled(format!("{label}: {message}"))
            }
            // Keep typed transport/deadline failures intact. In particular,
            // Timeout must remain retryable rather than becoming checker_failure.
            other => {
                eprintln!("{label}: {other}");
                other
            }
        }
    }
}
