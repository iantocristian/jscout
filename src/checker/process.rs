use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(test)]
use super::protocol::MemberResult;
use super::protocol::{
    Capabilities, Inbound, MemberBatchResult, MemberPlanResult, MemberQuery, Outbound,
    PROTOCOL_VERSION, ProjectValidationResult, Versions, encode,
};

const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
const INTERRUPTED_EXIT_CODE: i32 = 130;

static INTERRUPT_HANDLER: OnceLock<Result<(), String>> = OnceLock::new();
static INTERRUPT_CONTROL: Mutex<Option<CheckerControl>> = Mutex::new(None);
static INTERRUPT_PENDING: AtomicBool = AtomicBool::new(false);

#[derive(Debug)]
pub enum CheckerError {
    Spawn(String),
    Protocol(String),
    Io(String),
    ChildExited(String),
    Timeout(Duration),
    Canceled(String),
    Remote { code: String, message: String },
}

impl fmt::Display for CheckerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(message)
            | Self::Protocol(message)
            | Self::Io(message)
            | Self::ChildExited(message) => write!(formatter, "{message}"),
            Self::Timeout(timeout) => write!(formatter, "no checker reply within {timeout:?}"),
            Self::Canceled(reason) => write!(formatter, "checker request canceled ({reason})"),
            Self::Remote { code, message } => {
                write!(formatter, "checker error [{code}]: {message}")
            }
        }
    }
}

impl std::error::Error for CheckerError {}

struct Writer {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicU64,
}

impl Writer {
    fn send(&self, message: &Outbound) -> Result<String, CheckerError> {
        let id = format!("r{}", self.next_id.fetch_add(1, Ordering::Relaxed) + 1);
        let frame = encode(&id, message)
            .map_err(|error| CheckerError::Protocol(format!("encode failure: {error}")))?;
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| CheckerError::Io("checker stdin lock poisoned".into()))?;
        stdin
            .write_all(frame.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| CheckerError::ChildExited(format!("checker stdin closed: {error}")))?;
        Ok(id)
    }
}

#[derive(Clone)]
pub struct CheckerControl {
    writer: Arc<Writer>,
    active: Arc<Mutex<Option<String>>>,
}

impl CheckerControl {
    pub fn cancel_active(&self) -> Result<bool, CheckerError> {
        let target_id = self
            .active
            .lock()
            .map_err(|_| CheckerError::Io("checker active-request lock poisoned".into()))?
            .clone();
        let Some(target_id) = target_id else {
            return Ok(false);
        };
        self.writer.send(&Outbound::Cancel { target_id })?;
        Ok(true)
    }
}

fn install_interrupt_handler() -> Result<(), CheckerError> {
    let installation = INTERRUPT_HANDLER
        .get_or_init(|| ctrlc::set_handler(handle_interrupt).map_err(|error| error.to_string()));
    match installation {
        Ok(()) => Ok(()),
        Err(message) => Err(CheckerError::Spawn(format!(
            "failed to install Ctrl-C handler: {message}"
        ))),
    }
}

fn handle_interrupt() {
    if !request_interrupt_cancellation() {
        std::process::exit(INTERRUPTED_EXIT_CODE);
    }
}

fn request_interrupt_cancellation() -> bool {
    if INTERRUPT_PENDING.swap(true, Ordering::SeqCst) {
        return false;
    }
    let control = INTERRUPT_CONTROL
        .lock()
        .ok()
        .and_then(|registered| registered.clone());
    control
        .as_ref()
        .and_then(|control| control.cancel_active().ok())
        .unwrap_or(false)
}

/// Start one top-level checker operation. Per-project sidecars may replace the
/// active cancel target, but they must not clear an interrupt that already
/// canceled an earlier project in the same operation.
pub(crate) fn begin_interrupt_scope() -> Result<(), CheckerError> {
    install_interrupt_handler()?;
    INTERRUPT_PENDING.store(false, Ordering::SeqCst);
    Ok(())
}

pub(crate) fn interrupt_pending() -> bool {
    INTERRUPT_PENDING.load(Ordering::SeqCst)
}

fn register_interrupt_control(control: CheckerControl) -> Result<(), CheckerError> {
    install_interrupt_handler()?;
    *INTERRUPT_CONTROL
        .lock()
        .map_err(|_| CheckerError::Io("Ctrl-C control lock poisoned".into()))? = Some(control);
    Ok(())
}

fn unregister_interrupt_control(writer: &Arc<Writer>) {
    if let Ok(mut registered) = INTERRUPT_CONTROL.lock()
        && registered
            .as_ref()
            .is_some_and(|control| Arc::ptr_eq(&control.writer, writer))
    {
        *registered = None;
    }
}

pub struct ProcessChecker {
    child: Child,
    writer: Arc<Writer>,
    inbound: Receiver<Result<Inbound, CheckerError>>,
    active: Arc<Mutex<Option<String>>>,
    pub versions: Versions,
    poisoned: bool,
}

impl ProcessChecker {
    pub fn spawn(node: &Path, sidecar: &Path, root: &Path) -> Result<Self, CheckerError> {
        let mut child = Command::new(node)
            .arg(sidecar)
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                CheckerError::Spawn(format!(
                    "failed to launch `{} {} {}`: {error}",
                    node.display(),
                    sidecar.display(),
                    root.display()
                ))
            })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let (sender, inbound) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let message = match line {
                    Ok(text) if text.trim().is_empty() => continue,
                    Ok(text) => serde_json::from_str::<Inbound>(&text).map_err(|error| {
                        CheckerError::Protocol(format!("malformed checker message: {error}"))
                    }),
                    Err(error) => Err(CheckerError::Io(format!("checker stdout: {error}"))),
                };
                let failed = message.is_err();
                if sender.send(message).is_err() || failed {
                    return;
                }
            }
        });
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("typescript-checker: {line}");
            }
        });
        let mut checker = Self {
            child,
            writer: Arc::new(Writer {
                stdin: Mutex::new(stdin),
                next_id: AtomicU64::new(0),
            }),
            inbound,
            active: Arc::new(Mutex::new(None)),
            versions: Versions {
                sidecar: String::new(),
                node: String::new(),
                protocol: 0,
            },
            poisoned: false,
        };
        let id = checker.send(&Outbound::Hello)?;
        match checker.receive_for(&id, HELLO_TIMEOUT)? {
            Inbound::Ready { versions, .. } => {
                if versions.protocol != PROTOCOL_VERSION {
                    return Err(CheckerError::Protocol(format!(
                        "checker speaks protocol {}, jscout requires {PROTOCOL_VERSION}",
                        versions.protocol
                    )));
                }
                checker.versions = versions;
                Ok(checker)
            }
            Inbound::Error { error, .. } => Err(CheckerError::Remote {
                code: error.code,
                message: error.message,
            }),
            other => Err(unexpected("ready", &other)),
        }
    }

    pub fn control(&self) -> CheckerControl {
        CheckerControl {
            writer: Arc::clone(&self.writer),
            active: Arc::clone(&self.active),
        }
    }

    /// Point the process-wide Ctrl-C router at this sidecar. Registering a new
    /// per-project worker deliberately preserves the enclosing operation's
    /// interrupt state; `begin_interrupt_scope` resets it once per pass.
    pub fn register_interrupts(&self) -> Result<(), CheckerError> {
        register_interrupt_control(self.control())
    }

    pub fn capabilities(&mut self, timeout: Duration) -> Result<Capabilities, CheckerError> {
        let id = self.send(&Outbound::Capabilities)?;
        match self.receive_for(&id, timeout)? {
            Inbound::CapabilitiesResult { capabilities, .. } => Ok(capabilities),
            Inbound::Error { error, .. } => Err(CheckerError::Remote {
                code: error.code,
                message: error.message,
            }),
            other => Err(unexpected("capabilities_result", &other)),
        }
    }

    pub fn plan_members(
        &mut self,
        files: Vec<String>,
        timeout: Duration,
    ) -> Result<MemberPlanResult, CheckerError> {
        let id = self.send_active(&Outbound::PlanMembers { files })?;
        let result = match self.receive_for(&id, timeout) {
            Ok(Inbound::PlanMembersResult { result, .. }) => Ok(result),
            Ok(Inbound::Error { error, .. }) => Err(CheckerError::Remote {
                code: error.code,
                message: error.message,
            }),
            Ok(Inbound::Canceled { reason, .. }) => {
                Err(CheckerError::Canceled(reason.unwrap_or_default()))
            }
            Ok(other) => Err(unexpected("plan_members_result", &other)),
            Err(error) => Err(error),
        };
        self.clear_active();
        result
    }

    #[cfg(test)]
    pub fn resolve_member(
        &mut self,
        query: MemberQuery,
        timeout: Duration,
    ) -> Result<MemberResult, CheckerError> {
        let id = self.send_active(&Outbound::ResolveMember { query })?;
        let result = match self.receive_for(&id, timeout) {
            Ok(Inbound::ResolveMemberResult { result, .. }) => Ok(result),
            Ok(Inbound::Error { error, .. }) => Err(CheckerError::Remote {
                code: error.code,
                message: error.message,
            }),
            Ok(Inbound::Canceled { reason, .. }) => {
                Err(CheckerError::Canceled(reason.unwrap_or_default()))
            }
            Ok(other) => Err(unexpected("resolve_member_result", &other)),
            Err(error) => Err(error),
        };
        self.clear_active();
        result
    }

    pub fn resolve_members(
        &mut self,
        project_id: String,
        queries: Vec<MemberQuery>,
        timeout: Duration,
    ) -> Result<MemberBatchResult, CheckerError> {
        let id = self.send_active(&Outbound::ResolveMembers {
            project_id,
            queries,
        })?;
        let result = match self.receive_for(&id, timeout) {
            Ok(Inbound::ResolveMembersResult { result, .. }) => Ok(result),
            Ok(Inbound::Error { error, .. }) => Err(CheckerError::Remote {
                code: error.code,
                message: error.message,
            }),
            Ok(Inbound::Canceled { reason, .. }) => {
                Err(CheckerError::Canceled(reason.unwrap_or_default()))
            }
            Ok(other) => Err(unexpected("resolve_members_result", &other)),
            Err(error) => Err(error),
        };
        self.clear_active();
        result
    }

    pub fn validate_project(
        &mut self,
        project_id: String,
        fingerprint: String,
        timeout: Duration,
    ) -> Result<ProjectValidationResult, CheckerError> {
        let id = self.send_active(&Outbound::ValidateProject {
            project_id,
            fingerprint,
        })?;
        let result = match self.receive_for(&id, timeout) {
            Ok(Inbound::ValidateProjectResult { result, .. }) => Ok(result),
            Ok(Inbound::Error { error, .. }) => Err(CheckerError::Remote {
                code: error.code,
                message: error.message,
            }),
            Ok(Inbound::Canceled { reason, .. }) => {
                Err(CheckerError::Canceled(reason.unwrap_or_default()))
            }
            Ok(other) => Err(unexpected("validate_project_result", &other)),
            Err(error) => Err(error),
        };
        self.clear_active();
        result
    }

    fn send(&mut self, message: &Outbound) -> Result<String, CheckerError> {
        self.writer
            .send(message)
            .inspect_err(|_| self.poisoned = true)
    }

    fn send_active(&mut self, message: &Outbound) -> Result<String, CheckerError> {
        let id = self.send(message)?;
        *self
            .active
            .lock()
            .map_err(|_| CheckerError::Io("checker active-request lock poisoned".into()))? =
            Some(id.clone());
        Ok(id)
    }

    fn clear_active(&self) {
        if let Ok(mut active) = self.active.lock() {
            *active = None;
        }
    }

    fn receive_for(&mut self, id: &str, timeout: Duration) -> Result<Inbound, CheckerError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return self.timeout(timeout);
            }
            match self.inbound.recv_timeout(remaining) {
                Ok(Ok(message)) if message.id() == id => return Ok(message),
                Ok(Ok(Inbound::CancelResult {
                    target_id, active, ..
                })) => {
                    let _ = (target_id, active);
                    continue;
                }
                Ok(Ok(message)) => {
                    self.poisoned = true;
                    return Err(CheckerError::Protocol(format!(
                        "message for unexpected request id {}",
                        message.id()
                    )));
                }
                Ok(Err(error)) => {
                    self.poisoned = true;
                    return Err(error);
                }
                Err(RecvTimeoutError::Timeout) => return self.timeout(timeout),
                Err(RecvTimeoutError::Disconnected) => {
                    self.poisoned = true;
                    return Err(self.exit_error());
                }
            }
        }
    }

    fn timeout<T>(&mut self, timeout: Duration) -> Result<T, CheckerError> {
        self.poisoned = true;
        let _ = self.child.kill();
        let _ = self.child.wait();
        Err(CheckerError::Timeout(timeout))
    }

    fn exit_error(&mut self) -> CheckerError {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => format!("exited with {status}"),
            Ok(None) => "closed stdout while still running".into(),
            Err(error) => format!("could not be inspected: {error}"),
        };
        CheckerError::ChildExited(format!("checker {status}"))
    }
}

impl Drop for ProcessChecker {
    fn drop(&mut self) {
        unregister_interrupt_control(&self.writer);
        self.clear_active();
        if !self.poisoned {
            let _ = self.writer.send(&Outbound::Shutdown);
            let deadline = Instant::now() + SHUTDOWN_GRACE;
            while Instant::now() < deadline {
                if self.child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn unexpected(expected: &str, message: &Inbound) -> CheckerError {
    CheckerError::Protocol(format!(
        "expected {expected}, received message for {}",
        message.id()
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;
    use crate::checker::protocol::MemberQuery;

    // Canned protocol frames. Request IDs are deterministic per process: the
    // client numbers from r1, so hello is r1, the one resolve_member is r2, and
    // the cancel that follows it is r3.
    const READY: &str = r#"{"protocol":2,"id":"r1","kind":"ready","versions":{"sidecar":"fake","node":"22.19.0","protocol":2}}"#;
    const UNKNOWN_RESULT: &str = r#"{"protocol":2,"id":"r2","kind":"resolve_member_result","result":{"indexed_hash":"hash","source_hash":"hash","typescript":{"version":"5.9.3","source":"bundled"},"projects":[{"project_id":"inferred:a.ts","status":"unknown","declarations":[],"checker_input_fingerprint":"inputs"}],"configuration_problems":[]}}"#;
    const OUTSIDE_ERROR: &str = r#"{"protocol":2,"id":"r2","kind":"error","error":{"code":"outside_root","message":"outside root"}}"#;
    const CANCELED: &str = r#"{"protocol":2,"id":"r2","kind":"canceled","reason":"requested"}"#;
    const CANCEL_RESULT: &str =
        r#"{"protocol":2,"id":"r3","kind":"cancel_result","target_id":"r2","active":true}"#;
    const SHUTDOWN_RESULT: &str = r#"{"protocol":2,"id":"r3","kind":"shutdown_result"}"#;

    /// Write an executable fake sidecar (a `/bin/sh` script) answering the
    /// protocol from canned case patterns, and return it as the "node" binary
    /// plus the sidecar path. Following the gateway's fake-process precedent
    /// keeps the default `cargo test` suite runnable without Node installed:
    /// these tests exercise the Rust client, not the TypeScript worker.
    fn fake_sidecar(mode: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let directory = tempfile::tempdir().expect("tempdir");
        let script = directory.path().join("fake-checker.sh");
        // `timeout` and `cancel` answer a query with silence.
        let resolve = match mode {
            "unknown" => format!("echo '{UNKNOWN_RESULT}'"),
            "outside" => format!("echo '{OUTSIDE_ERROR}'"),
            "crash" => "exit 3".to_string(),
            _ => ":".to_string(),
        };
        let source = format!(
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"resolve_member"'*) {resolve} ;;
    *'"kind":"cancel"'*) echo '{CANCELED}'; echo '{CANCEL_RESULT}' ;;
    *'"kind":"shutdown"'*) echo '{SHUTDOWN_RESULT}'; exit 0 ;;
  esac
done
"#
        );
        fs::write(&script, source).expect("fake sidecar");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("executable");
        (directory, script)
    }

    fn query() -> MemberQuery {
        MemberQuery {
            file: "a.ts".into(),
            indexed_hash: "hash".into(),
            call_start: 1,
            call_end: 10,
            receiver_start: 1,
            receiver_end: 4,
            property_start: 5,
            property_end: 8,
        }
    }

    fn spawn_fake(mode: &str) -> (tempfile::TempDir, ProcessChecker) {
        let (directory, script) = fake_sidecar(mode);
        let checker = ProcessChecker::spawn(Path::new("/bin/sh"), &script, directory.path())
            .expect("spawn fake");
        (directory, checker)
    }

    #[test]
    fn fake_sidecar_preserves_unknown_and_stable_remote_errors() {
        let (_directory, mut checker) = spawn_fake("unknown");
        let answer = checker
            .resolve_member(query(), Duration::from_secs(1))
            .expect("unknown response");
        assert_eq!(answer.projects[0].status, "unknown");
        drop(checker);

        let (_directory, mut checker) = spawn_fake("outside");
        let error = checker
            .resolve_member(query(), Duration::from_secs(1))
            .expect_err("outside root");
        assert!(matches!(error, CheckerError::Remote { code, .. } if code == "outside_root"));
    }

    #[test]
    fn timeout_terminates_the_unresponsive_sidecar() {
        let (_directory, mut checker) = spawn_fake("timeout");
        let error = checker
            .resolve_member(query(), Duration::from_millis(30))
            .expect_err("timeout");
        assert!(matches!(error, CheckerError::Timeout(_)));
        assert!(checker.child.try_wait().expect("child status").is_some());
    }

    #[test]
    fn crash_and_cancel_are_distinct_terminal_outcomes() {
        let (_directory, mut checker) = spawn_fake("crash");
        let error = checker
            .resolve_member(query(), Duration::from_secs(1))
            .expect_err("crash");
        assert!(matches!(error, CheckerError::ChildExited(_)));
        drop(checker);

        let (_directory, mut checker) = spawn_fake("cancel");
        let control = checker.control();
        let canceler = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            control.cancel_active()
        });
        let error = checker
            .resolve_member(query(), Duration::from_secs(1))
            .expect_err("canceled");
        assert!(canceler.join().expect("cancel thread").expect("cancel"));
        assert!(matches!(error, CheckerError::Canceled(reason) if reason == "requested"));
    }
}
