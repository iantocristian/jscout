use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::protocol::{
    Capabilities, Inbound, InputValidation, MemberQuery, MemberResult, Outbound, PROTOCOL_VERSION,
    ValidationResult, Versions, encode,
};

const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);

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

    pub fn validate_inputs(
        &mut self,
        entries: Vec<InputValidation>,
        timeout: Duration,
    ) -> Result<ValidationResult, CheckerError> {
        let id = self.send_active(&Outbound::ValidateInputs { entries })?;
        let result = match self.receive_for(&id, timeout) {
            Ok(Inbound::ValidateInputsResult { result, .. }) => Ok(result),
            Ok(Inbound::Error { error, .. }) => Err(CheckerError::Remote {
                code: error.code,
                message: error.message,
            }),
            Ok(Inbound::Canceled { reason, .. }) => {
                Err(CheckerError::Canceled(reason.unwrap_or_default()))
            }
            Ok(other) => Err(unexpected("validate_inputs_result", &other)),
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
