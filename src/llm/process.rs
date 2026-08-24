//! Child-process gateway client: spawn `node <gateway-path>` with argument
//! arrays, speak versioned JSONL over its stdio, and fail the current request
//! on EOF, malformed frames, timeouts, or an unexpected exit. The child never
//! outlives the client: drop sends `shutdown`, then kills after a grace
//! period.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use std::time::Instant;

use anyhow::Context;

use super::config;
use super::protocol::{
    CompleteRequest, GatewayVersions, Inbound, ModelCapabilities, Outbound, PROTOCOL_VERSION,
    ProviderSummary, encode,
};
use super::{CompletionOutcome, CompletionTask, GatewayError, LlmGateway, StartedInfo};

const HELLO_TIMEOUT: Duration = Duration::from_secs(30);
const SHUTDOWN_GRACE: Duration = Duration::from_millis(500);
const INTERRUPTED_EXIT_CODE: i32 = 130;

static INTERRUPT_HANDLER: OnceLock<Result<(), String>> = OnceLock::new();
static INTERRUPT_CONTROL: Mutex<Option<InterruptControl>> = Mutex::new(None);
static INTERRUPT_PENDING: AtomicBool = AtomicBool::new(false);
static INTERRUPT_GENERATION: AtomicU64 = AtomicU64::new(0);

pub struct ProcessGateway {
    child: Child,
    writer: Arc<GatewayWriter>,
    cancel_sender: Sender<String>,
    inbound: Receiver<Result<Inbound, GatewayError>>,
    active_request: Arc<Mutex<Option<String>>>,
    pub versions: GatewayVersions,
    poisoned: bool,
}

/// A fixed set of one-request gateway processes. Scouting uses one worker by
/// default; a larger configured pool overlaps provider waits without making
/// the semantic database a concurrent-write surface.
pub struct ProcessGatewayPool {
    workers: Vec<ProcessGateway>,
}

struct GatewayWriter {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicU64,
}

/// Independent cancellation handle. The writer handle identifies the
/// registered gateway; cancellation itself is queued so signal handling never
/// waits on child stdin.
#[derive(Clone)]
pub struct GatewayControl {
    writer: Arc<GatewayWriter>,
    cancel_sender: Sender<String>,
    active_request: Arc<Mutex<Option<String>>>,
}

#[derive(Clone)]
struct InterruptControl {
    gateways: Vec<GatewayControl>,
}

/// Interrupt state captured before a completion or bounded batch is
/// dispatched. The generation remains latched after active requests finish,
/// so a delayed worker from the same batch cannot mistake a cleared pending
/// bit for permission to start.
#[derive(Clone, Copy)]
struct DispatchAdmission {
    generation: u64,
    interrupted: bool,
}

impl GatewayControl {
    /// Queue cancellation for the current completion, if one is active.
    pub fn cancel_active(&self) -> Result<bool, GatewayError> {
        let target_id = self
            .active_request
            .lock()
            .map_err(|_| GatewayError::Io("gateway active-request lock poisoned".into()))?
            .clone();
        let Some(target_id) = target_id else {
            return Ok(false);
        };
        self.cancel_sender
            .send(target_id)
            .map_err(|_| GatewayError::ChildExited("gateway cancellation queue closed".into()))?;
        Ok(true)
    }
}

impl InterruptControl {
    fn contains(&self, writer: &Arc<GatewayWriter>) -> bool {
        self.gateways
            .iter()
            .any(|gateway| Arc::ptr_eq(&gateway.writer, writer))
    }

    fn enqueue_active_cancellations(&self) -> bool {
        let mut queued = false;
        for gateway in &self.gateways {
            if let Ok(active) = gateway.cancel_active() {
                queued |= active;
            }
        }
        queued
    }

    fn any_active(&self) -> bool {
        self.gateways.iter().any(|gateway| {
            gateway
                .active_request
                .lock()
                .ok()
                .is_some_and(|active| active.is_some())
        })
    }
}

impl DispatchAdmission {
    fn capture() -> Result<Self, GatewayError> {
        let _gate = INTERRUPT_CONTROL
            .lock()
            .map_err(|_| GatewayError::Io("Ctrl-C control lock poisoned".into()))?;
        Ok(Self {
            generation: INTERRUPT_GENERATION.load(Ordering::SeqCst),
            interrupted: INTERRUPT_PENDING.load(Ordering::SeqCst),
        })
    }
}

fn install_interrupt_handler() -> Result<(), GatewayError> {
    let installation = INTERRUPT_HANDLER
        .get_or_init(|| ctrlc::set_handler(handle_interrupt).map_err(|error| error.to_string()));
    match installation {
        Ok(()) => Ok(()),
        Err(message) => Err(GatewayError::Spawn(format!(
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
    // The pending transition shares the admission gate with send_complete.
    // Either a completion publishes its id first and this snapshot cancels it,
    // or this transition wins and that completion is refused before sending.
    let handled = match INTERRUPT_CONTROL.lock() {
        Ok(registered) => {
            if INTERRUPT_PENDING.swap(true, Ordering::SeqCst) {
                return false;
            }
            INTERRUPT_GENERATION.fetch_add(1, Ordering::SeqCst);
            match registered.as_ref() {
                Some(control) => control.enqueue_active_cancellations(),
                None => false,
            }
        }
        Err(_) => {
            if INTERRUPT_PENDING.swap(true, Ordering::SeqCst) {
                return false;
            }
            INTERRUPT_GENERATION.fetch_add(1, Ordering::SeqCst);
            false
        }
    };
    // The Ctrl-C thread only enqueues target ids. A dedicated per-gateway
    // writer serializes Complete before Cancel and may wait on stdin without
    // preventing this handler from processing a second interrupt.
    handled
}

fn register_interrupt_controls(controls: Vec<GatewayControl>) -> Result<(), GatewayError> {
    install_interrupt_handler()?;
    let mut registered = INTERRUPT_CONTROL
        .lock()
        .map_err(|_| GatewayError::Io("Ctrl-C control lock poisoned".into()))?;
    *registered = Some(InterruptControl { gateways: controls });
    INTERRUPT_PENDING.store(false, Ordering::SeqCst);
    Ok(())
}

fn unregister_interrupt_control(writer: &Arc<GatewayWriter>) {
    if let Ok(mut registered) = INTERRUPT_CONTROL.lock()
        && registered
            .as_ref()
            .is_some_and(|control| control.contains(writer))
    {
        *registered = None;
        INTERRUPT_PENDING.store(false, Ordering::SeqCst);
    }
}

impl GatewayWriter {
    fn prepare(&self, message: &Outbound) -> Result<(String, String), GatewayError> {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let id = format!("r{sequence}");
        let line = encode(&id, message)
            .map_err(|error| GatewayError::Protocol(format!("encode failure: {error}")))?;
        Ok((id, line))
    }

    fn write_line(stdin: &mut ChildStdin, line: &str) -> Result<(), GatewayError> {
        stdin
            .write_all(line.as_bytes())
            .and_then(|()| stdin.write_all(b"\n"))
            .and_then(|()| stdin.flush())
            .map_err(|error| GatewayError::ChildExited(format!("gateway stdin closed: {error}")))
    }

    fn send(&self, message: &Outbound) -> Result<String, GatewayError> {
        let (id, line) = self.prepare(message)?;
        let mut stdin = self
            .stdin
            .lock()
            .map_err(|_| GatewayError::Io("gateway stdin lock poisoned".into()))?;
        Self::write_line(&mut stdin, &line)?;
        Ok(id)
    }
}

struct ActiveRequestGuard {
    writer: Arc<GatewayWriter>,
    active_request: Arc<Mutex<Option<String>>>,
}

impl ActiveRequestGuard {
    fn new(writer: Arc<GatewayWriter>, active_request: Arc<Mutex<Option<String>>>) -> Self {
        Self {
            writer,
            active_request,
        }
    }
}

impl Drop for ActiveRequestGuard {
    fn drop(&mut self) {
        if let Ok(registered) = INTERRUPT_CONTROL.lock() {
            if let Ok(mut active) = self.active_request.lock() {
                *active = None;
            }
            if registered
                .as_ref()
                .is_some_and(|control| control.contains(&self.writer) && !control.any_active())
            {
                INTERRUPT_PENDING.store(false, Ordering::SeqCst);
            }
        } else if let Ok(mut active) = self.active_request.lock() {
            *active = None;
        }
    }
}

fn gateway_environment(
    runtime: &crate::config::RuntimeConfig,
) -> anyhow::Result<Vec<(OsString, OsString)>> {
    let settings = &runtime.effective.llm;
    let mut environment = vec![(
        OsString::from("JSCOUT_PI_AI_AUTH_FILE"),
        settings.auth_file.as_os_str().to_os_string(),
    )];
    if let Some(base_url) = &settings.openai_base_url {
        environment.push((
            OsString::from("JSCOUT_PI_AI_OPENAI_BASE_URL"),
            OsString::from(base_url),
        ));
    }
    if !settings.openai_compatible_providers.is_empty() {
        environment.push((
            OsString::from("JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS"),
            OsString::from(serde_json::to_string(
                &settings.openai_compatible_providers,
            )?),
        ));
    }
    if settings.api_key_env != "OPENAI_API_KEY" {
        let value = std::env::var_os(&settings.api_key_env).with_context(|| {
            format!(
                "llm.api_key_env references missing secret environment {}",
                settings.api_key_env
            )
        })?;
        environment.push((OsString::from("OPENAI_API_KEY"), value));
    }
    Ok(environment)
}

impl ProcessGateway {
    /// Spawn and complete the `hello`/`ready` handshake.
    #[cfg(test)]
    pub fn spawn(node: &Path, gateway: &Path) -> Result<Self, GatewayError> {
        Self::spawn_with_environment(node, gateway, &[])
    }

    fn spawn_with_environment(
        node: &Path,
        gateway: &Path,
        environment: &[(OsString, OsString)],
    ) -> Result<Self, GatewayError> {
        let mut command = Command::new(node);
        command
            .arg(gateway)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (name, value) in environment {
            command.env(name, value);
        }
        let mut child = command.spawn().map_err(|error| {
            GatewayError::Spawn(format!(
                "failed to launch `{} {}`: {error}",
                node.display(),
                gateway.display()
            ))
        })?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");

        let (sender, inbound) = channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                let message = match line {
                    Ok(text) if text.trim().is_empty() => continue,
                    Ok(text) => serde_json::from_str::<Inbound>(&text).map_err(|error| {
                        GatewayError::Protocol(format!("malformed gateway message: {error}"))
                    }),
                    Err(error) => Err(GatewayError::Io(format!("gateway stdout: {error}"))),
                };
                let failed = message.is_err();
                if sender.send(message).is_err() || failed {
                    return;
                }
            }
            // EOF: the receiver observes the disconnect as ChildExited.
        });
        std::thread::spawn(move || {
            // Sanitized diagnostics only; forwarded line-by-line for doctor
            // output and operator logs.
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                eprintln!("pi-ai-gateway: {line}");
            }
        });

        let writer = Arc::new(GatewayWriter {
            stdin: Mutex::new(stdin),
            next_id: AtomicU64::new(0),
        });
        let (cancel_sender, cancel_receiver) = channel();
        let cancel_writer = Arc::clone(&writer);
        std::thread::spawn(move || {
            while let Ok(target_id) = cancel_receiver.recv() {
                let _ = cancel_writer.send(&Outbound::Cancel { target_id });
            }
        });

        let mut gateway_client = Self {
            child,
            writer,
            cancel_sender,
            inbound,
            active_request: Arc::new(Mutex::new(None)),
            versions: GatewayVersions {
                gateway: String::new(),
                pi_ai: String::new(),
                node: String::new(),
                protocol: 0,
            },
            poisoned: false,
        };
        let id = gateway_client.send(&Outbound::Hello)?;
        match gateway_client.receive_for(&id, HELLO_TIMEOUT)? {
            Inbound::Ready { versions, .. } => {
                if versions.protocol != PROTOCOL_VERSION {
                    return Err(GatewayError::Protocol(format!(
                        "gateway speaks protocol {}, this jscout requires {PROTOCOL_VERSION}",
                        versions.protocol
                    )));
                }
                gateway_client.versions = versions;
                Ok(gateway_client)
            }
            Inbound::Error { error, .. } => Err(GatewayError::from_remote(error)),
            other => Err(unexpected("ready", &other)),
        }
    }

    /// Convenience: resolve node + gateway from config and spawn.
    pub fn launch(
        gateway_path: Option<&Path>,
        runtime: &crate::config::RuntimeConfig,
    ) -> anyhow::Result<Self> {
        let node =
            config::resolve_node_setting(&runtime.effective.sidecars.node, "the pi-ai gateway")?;
        config::verify_node_version(&node)?;
        let gateway = config::resolve_gateway_setting(
            gateway_path,
            runtime.effective.sidecars.gateway.as_deref(),
        )?;
        let environment = gateway_environment(runtime)?;
        let client = Self::spawn_with_environment(&node, &gateway, &environment)?;
        register_interrupt_controls(vec![client.control()])?;
        Ok(client)
    }

    fn send(&mut self, message: &Outbound) -> Result<String, GatewayError> {
        self.writer
            .send(message)
            .inspect_err(|_| self.poisoned = true)
    }

    /// Serialize this completion ahead of any queued cancel, then publish its
    /// cancel target under the interrupt admission gate. The potentially
    /// blocking pipe write happens only after the global and active-id locks
    /// have been released.
    fn send_complete(
        &mut self,
        request: &CompleteRequest,
        admission: DispatchAdmission,
    ) -> Result<(String, ActiveRequestGuard), GatewayError> {
        let writer = Arc::clone(&self.writer);
        let (id, line) = writer
            .prepare(&Outbound::Complete(Box::new(request.clone())))
            .inspect_err(|_| self.poisoned = true)?;
        // Taking stdin first guarantees a cancellation queued after active-id
        // publication cannot overtake the Complete frame. This wait is outside
        // INTERRUPT_CONTROL, so it cannot stall the Ctrl-C callback.
        let mut stdin = writer
            .stdin
            .lock()
            .map_err(|_| GatewayError::Io("gateway stdin lock poisoned".into()))?;
        let interrupt_gate = INTERRUPT_CONTROL
            .lock()
            .map_err(|_| GatewayError::Io("Ctrl-C control lock poisoned".into()))?;
        let interrupt_applies = interrupt_gate
            .as_ref()
            .is_some_and(|control| control.contains(&self.writer));
        let interrupt_generation = INTERRUPT_GENERATION.load(Ordering::SeqCst);
        if interrupt_applies
            && (admission.interrupted
                || INTERRUPT_PENDING.load(Ordering::SeqCst)
                || admission.generation != interrupt_generation)
        {
            return Err(GatewayError::Canceled(
                "interrupted before gateway dispatch".into(),
            ));
        }
        let active_request = Arc::clone(&self.active_request);
        let mut active = active_request
            .lock()
            .map_err(|_| GatewayError::Io("gateway active-request lock poisoned".into()))?;
        *active = Some(id.clone());
        drop(active);
        let active_guard = ActiveRequestGuard::new(Arc::clone(&writer), active_request);
        drop(interrupt_gate);
        if let Err(error) = GatewayWriter::write_line(&mut stdin, &line) {
            self.poisoned = true;
            return Err(error);
        }
        Ok((id, active_guard))
    }

    /// Receive the next message for `id` within `timeout`. Messages for other
    /// ids indicate a protocol bug under the one-at-a-time contract and fail
    /// the request instead of being silently dropped.
    fn receive_for(&mut self, id: &str, timeout: Duration) -> Result<Inbound, GatewayError> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                self.poisoned = true;
                return Err(GatewayError::Timeout(timeout));
            }
            match self.inbound.recv_timeout(remaining) {
                Ok(Ok(message)) if message.id() == id => return Ok(message),
                // Control handles do not wait on acknowledgements; consume
                // them here while preserving the completion's total timeout.
                // A positive acknowledgement can only name the completion
                // currently awaited by this one-at-a-time client. Negative
                // acknowledgements may be left over from a completion race.
                Ok(Ok(Inbound::CancelResult {
                    target_id, active, ..
                })) => {
                    if active && target_id != id {
                        self.poisoned = true;
                        return Err(GatewayError::Protocol(format!(
                            "cancel acknowledgement activated request {target_id}, but the client is awaiting {id}"
                        )));
                    }
                    continue;
                }
                Ok(Ok(message)) => {
                    self.poisoned = true;
                    return Err(GatewayError::Protocol(format!(
                        "message for unexpected request id {}",
                        message.id()
                    )));
                }
                Ok(Err(error)) => {
                    self.poisoned = true;
                    return Err(error);
                }
                Err(RecvTimeoutError::Timeout) => {
                    self.poisoned = true;
                    return Err(GatewayError::Timeout(timeout));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    self.poisoned = true;
                    return Err(self.exit_error());
                }
            }
        }
    }

    fn exit_error(&mut self) -> GatewayError {
        let status = match self.child.try_wait() {
            Ok(Some(status)) => format!("exited with {status}"),
            Ok(None) => "closed its stdout while still running".to_string(),
            Err(error) => format!("could not be inspected: {error}"),
        };
        GatewayError::ChildExited(format!("gateway {status}"))
    }

    /// A poisoned client saw a framing/timeout failure and can no longer
    /// trust request correlation; callers must discard it.
    #[cfg(test)]
    pub fn poisoned(&self) -> bool {
        self.poisoned
    }

    pub fn control(&self) -> GatewayControl {
        GatewayControl {
            writer: Arc::clone(&self.writer),
            cancel_sender: self.cancel_sender.clone(),
            active_request: Arc::clone(&self.active_request),
        }
    }
}

impl ProcessGatewayPool {
    /// Launch exactly the configured number of independent gateway workers.
    /// Configuration validates the value and deliberately applies no upper
    /// clamp: increasing concurrency is an explicit operator choice.
    pub fn launch(
        gateway_path: Option<&Path>,
        runtime: &crate::config::RuntimeConfig,
        max_concurrency: usize,
    ) -> anyhow::Result<Self> {
        if max_concurrency == 0 {
            anyhow::bail!("llm.max_concurrency must be greater than zero");
        }
        let node =
            config::resolve_node_setting(&runtime.effective.sidecars.node, "the pi-ai gateway")?;
        config::verify_node_version(&node)?;
        let gateway = config::resolve_gateway_setting(
            gateway_path,
            runtime.effective.sidecars.gateway.as_deref(),
        )?;
        let environment = gateway_environment(runtime)?;
        let mut workers = Vec::with_capacity(max_concurrency);
        for _ in 0..max_concurrency {
            workers.push(ProcessGateway::spawn_with_environment(
                &node,
                &gateway,
                &environment,
            )?);
        }
        register_interrupt_controls(workers.iter().map(ProcessGateway::control).collect())?;
        Ok(Self { workers })
    }
}

impl LlmGateway for ProcessGatewayPool {
    fn capabilities(
        &mut self,
        model: Option<&str>,
    ) -> Result<(ProviderSummary, Option<ModelCapabilities>), GatewayError> {
        self.workers[0].capabilities(model)
    }

    fn complete(
        &mut self,
        request: &CompleteRequest,
        timeout: Duration,
    ) -> Result<CompletionOutcome, GatewayError> {
        self.workers[0].complete(request, timeout)
    }

    fn complete_batch(
        &mut self,
        tasks: &[CompletionTask<'_>],
    ) -> Vec<Result<CompletionOutcome, GatewayError>> {
        let admission = match DispatchAdmission::capture() {
            Ok(admission) => admission,
            Err(error) => {
                let message = error.to_string();
                return tasks
                    .iter()
                    .map(|_| Err(GatewayError::Io(message.clone())))
                    .collect();
            }
        };
        let mut outcomes = Vec::with_capacity(tasks.len());
        for batch in tasks.chunks(self.workers.len()) {
            let batch_outcomes = std::thread::scope(|scope| {
                let handles = self
                    .workers
                    .iter_mut()
                    .zip(batch)
                    .map(|(worker, task)| {
                        scope.spawn(move || {
                            worker.complete_with_admission(task.request, task.timeout, admission)
                        })
                    })
                    .collect::<Vec<_>>();
                handles
                    .into_iter()
                    .map(|handle| {
                        handle.join().unwrap_or_else(|_| {
                            Err(GatewayError::Io("gateway worker thread panicked".into()))
                        })
                    })
                    .collect::<Vec<_>>()
            });
            outcomes.extend(batch_outcomes);
        }
        outcomes
    }
}

impl LlmGateway for ProcessGateway {
    fn capabilities(
        &mut self,
        model: Option<&str>,
    ) -> Result<(ProviderSummary, Option<ModelCapabilities>), GatewayError> {
        let id = self.send(&Outbound::Capabilities {
            model: model.map(str::to_string),
        })?;
        match self.receive_for(&id, HELLO_TIMEOUT)? {
            Inbound::CapabilitiesResult {
                providers, model, ..
            } => Ok((providers, model)),
            Inbound::Error { error, .. } => Err(GatewayError::from_remote(error)),
            other => Err(unexpected("capabilities_result", &other)),
        }
    }

    fn complete(
        &mut self,
        request: &CompleteRequest,
        timeout: Duration,
    ) -> Result<CompletionOutcome, GatewayError> {
        // The gateway enforces the request timeout; the client allows a grace
        // margin so the remote timeout error arrives instead of a local one.
        self.complete_with_grace(request, timeout, Duration::from_secs(5))
    }
}

impl ProcessGateway {
    fn complete_with_admission(
        &mut self,
        request: &CompleteRequest,
        timeout: Duration,
        admission: DispatchAdmission,
    ) -> Result<CompletionOutcome, GatewayError> {
        self.complete_with_grace_and_admission(request, timeout, Duration::from_secs(5), admission)
    }

    fn complete_with_grace(
        &mut self,
        request: &CompleteRequest,
        timeout: Duration,
        grace: Duration,
    ) -> Result<CompletionOutcome, GatewayError> {
        let admission = DispatchAdmission::capture()?;
        self.complete_with_grace_and_admission(request, timeout, grace, admission)
    }

    fn complete_with_grace_and_admission(
        &mut self,
        request: &CompleteRequest,
        timeout: Duration,
        grace: Duration,
        admission: DispatchAdmission,
    ) -> Result<CompletionOutcome, GatewayError> {
        let (id, _active) = self.send_complete(request, admission)?;
        let wire_timeout = timeout + grace;
        let started = match self.receive_for(&id, wire_timeout)? {
            Inbound::Started {
                provider,
                model,
                api,
                base_url,
                billing_path,
                auth_source,
                ..
            } => StartedInfo {
                provider,
                model,
                api,
                base_url,
                billing_path,
                auth_source,
            },
            Inbound::Error { error, .. } => return Err(GatewayError::from_remote(error)),
            Inbound::Canceled { reason, .. } => {
                return Err(GatewayError::Canceled(reason.unwrap_or_default()));
            }
            other => return Err(unexpected("started", &other)),
        };
        match self.receive_for(&id, wire_timeout)? {
            Inbound::Result {
                tool_call,
                stop_reason,
                usage,
                attempts,
                response_model,
                ..
            } => Ok(CompletionOutcome {
                started,
                tool_call,
                stop_reason,
                usage,
                attempts,
                response_model,
            }),
            Inbound::Error { error, .. } => Err(GatewayError::from_remote(error)),
            Inbound::Canceled { reason, .. } => {
                Err(GatewayError::Canceled(reason.unwrap_or_default()))
            }
            other => Err(unexpected("result", &other)),
        }
    }
}

impl Drop for ProcessGateway {
    fn drop(&mut self) {
        unregister_interrupt_control(&self.writer);
        if !self.poisoned {
            let _ = self.send(&Outbound::Shutdown);
            let deadline = Instant::now() + SHUTDOWN_GRACE;
            while Instant::now() < deadline {
                if matches!(self.child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn unexpected(expected: &str, actual: &Inbound) -> GatewayError {
    GatewayError::Protocol(format!(
        "expected {expected}, received {} for request {}",
        kind_name(actual),
        actual.id()
    ))
}

fn kind_name(message: &Inbound) -> &'static str {
    match message {
        Inbound::Ready { .. } => "ready",
        Inbound::CapabilitiesResult { .. } => "capabilities_result",
        Inbound::Started { .. } => "started",
        Inbound::Result { .. } => "result",
        Inbound::Error { .. } => "error",
        Inbound::Canceled { .. } => "canceled",
        Inbound::CancelResult { .. } => "cancel_result",
        Inbound::ShutdownResult { .. } => "shutdown_result",
    }
}

#[cfg(test)]
pub use fake::write_fake_gateway;

#[cfg(test)]
mod fake {
    use std::path::{Path, PathBuf};

    /// Write an executable fake gateway (a /bin/sh script) that answers the
    /// protocol from canned case patterns. Tests inject it as the "node"
    /// binary with the script as the gateway path, so the process client is
    /// exercised without Node or network access.
    pub fn write_fake_gateway(dir: &Path, body: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("fake-gateway.sh");
        std::fs::write(&script, format!("#!/bin/sh\n{body}"))?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        Ok((PathBuf::from("/bin/sh"), script))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, mpsc};
    use std::thread;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use super::super::protocol::{ChatMessage, CompleteRequest, SubmitTool};
    use super::super::{CompletionTask, GatewayError, LlmGateway};
    use super::{
        DispatchAdmission, INTERRUPT_PENDING, ProcessGateway, ProcessGatewayPool,
        gateway_environment, register_interrupt_controls, request_interrupt_cancellation,
        write_fake_gateway,
    };

    static INTERRUPT_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn gateway_environment_contains_typed_non_secret_runtime_configuration() -> anyhow::Result<()> {
        let mut runtime = crate::config::RuntimeConfig::load(None, None)?;
        runtime.effective.llm.openai_base_url = Some("https://gateway.example.test/v1".to_string());
        runtime.effective.llm.api_key_env = "OPENAI_API_KEY".to_string();
        runtime.effective.llm.openai_compatible_providers =
            vec![crate::config::OpenAiCompatibleProvider {
                id: "local".to_string(),
                name: "Local".to_string(),
                base_url: "http://127.0.0.1:1234/v1".to_string(),
                api_key_env: Some("LOCAL_MODEL_KEY".to_string()),
                models: vec![crate::config::OpenAiCompatibleModel {
                    id: "model".to_string(),
                    name: "Model".to_string(),
                    reasoning: false,
                    context_window: 8_192,
                    max_tokens: 2_048,
                }],
            }];
        let environment = gateway_environment(&runtime)?;
        let environment = environment
            .into_iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment.get("JSCOUT_PI_AI_OPENAI_BASE_URL"),
            Some(&"https://gateway.example.test/v1".to_string())
        );
        let providers: serde_json::Value = serde_json::from_str(
            environment
                .get("JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS")
                .expect("compatible providers"),
        )?;
        assert_eq!(providers[0]["apiKeyEnv"], "LOCAL_MODEL_KEY");
        assert!(!environment.contains_key("OPENAI_API_KEY"));
        Ok(())
    }

    const READY: &str = r#"{"protocol":1,"id":"r1","kind":"ready","versions":{"gateway":"0.0.0","pi_ai":"0.0.0","node":"22.19.0","protocol":1}}"#;

    fn complete_request() -> CompleteRequest {
        CompleteRequest {
            model: "faux:faux-model".into(),
            reasoning: None,
            system: Some("system".into()),
            messages: vec![ChatMessage {
                role: "user",
                content: "hello".into(),
            }],
            tool: SubmitTool {
                name: "submit".into(),
                description: "submit".into(),
                parameters: json!({"type": "object"}),
            },
            timeout_ms: Some(5_000),
            max_tokens: None,
            session_id: None,
            provider_options: None,
        }
    }

    fn spawn_with(body: &str) -> anyhow::Result<ProcessGateway> {
        let dir = tempfile::tempdir()?;
        let (node, script) = write_fake_gateway(dir.path(), body)?;
        let gateway = ProcessGateway::spawn(&node, &script)?;
        // The tempdir may be deleted once the script is running.
        drop(dir);
        Ok(gateway)
    }

    fn wait_until_active(control: &super::GatewayControl) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if control
                .active_request
                .lock()
                .map_err(|_| anyhow::anyhow!("gateway active-request lock poisoned"))?
                .is_some()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                anyhow::bail!("gateway did not publish its active request");
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn completes_through_a_fake_gateway_and_shuts_down() -> anyhow::Result<()> {
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*)
      echo '{{"protocol":1,"id":"r2","kind":"started","provider":"faux","model":"faux-model","api":"faux","billing_path":"api","auth_source":"test"}}'
      echo '{{"protocol":1,"id":"r2","kind":"result","tool_call":{{"name":"submit","arguments":{{"ok":true}}}},"stop_reason":"toolUse","usage":{{"input_tokens":1,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"total_tokens":3,"cost_total":0}},"response_model":"faux-model"}}'
      ;;
    *'"kind":"shutdown"'*) echo '{{"protocol":1,"id":"r3","kind":"shutdown_result"}}'; exit 0 ;;
  esac
done"#
        );
        let mut gateway = spawn_with(&body)?;
        assert_eq!(gateway.versions.node, "22.19.0");
        let outcome = gateway.complete(&complete_request(), Duration::from_secs(5))?;
        assert_eq!(outcome.started.billing_path, "api");
        assert_eq!(outcome.tool_call.name, "submit");
        assert_eq!(outcome.tool_call.arguments, json!({"ok": true}));
        assert_eq!(outcome.usage.total_tokens, 3);
        assert_eq!(outcome.attempts, 1);
        drop(gateway); // shutdown path must not hang
        Ok(())
    }

    #[test]
    fn gateway_pool_overlaps_independent_completions() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let markers = dir.path().join("markers");
        std::fs::create_dir(&markers)?;
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*)
      touch '{markers}/'$$
      while [ "$(find '{markers}' -type f | wc -l | tr -d ' ')" -lt 2 ]; do sleep 0.01; done
      echo '{{"protocol":1,"id":"r2","kind":"started","provider":"faux","model":"faux-model","api":"faux","billing_path":"api","auth_source":"test"}}'
      echo '{{"protocol":1,"id":"r2","kind":"result","tool_call":{{"name":"submit","arguments":{{"ok":true}}}},"stop_reason":"toolUse","usage":{{"input_tokens":1,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"total_tokens":3,"cost_total":0}}}}'
      ;;
    *'"kind":"shutdown"'*) echo '{{"protocol":1,"id":"r3","kind":"shutdown_result"}}'; exit 0 ;;
  esac
done"#,
            markers = markers.display(),
        );
        let (node, script) = write_fake_gateway(dir.path(), &body)?;
        let workers = vec![
            ProcessGateway::spawn(&node, &script)?,
            ProcessGateway::spawn(&node, &script)?,
        ];
        let mut pool = ProcessGatewayPool { workers };
        let requests = [complete_request(), complete_request()];
        let tasks = requests
            .iter()
            .map(|request| CompletionTask {
                request,
                timeout: Duration::from_secs(2),
            })
            .collect::<Vec<_>>();
        let outcomes = pool.complete_batch(&tasks);
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.into_iter().all(|outcome| outcome.is_ok()));
        Ok(())
    }

    #[test]
    fn interrupt_rejects_a_delayed_worker_from_the_same_batch() -> anyhow::Result<()> {
        let _interrupt_test = INTERRUPT_TEST_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("interrupt test lock poisoned"))?;
        let dir = tempfile::tempdir()?;
        let late_dispatch = dir.path().join("late-dispatch");
        let first_dir = tempfile::tempdir_in(dir.path())?;
        let second_dir = tempfile::tempdir_in(dir.path())?;
        let first_body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*)
      echo '{{"protocol":1,"id":"r2","kind":"started","provider":"faux","model":"faux-model","api":"faux","billing_path":"api","auth_source":"test"}}'
      ;;
    *'"kind":"cancel"'*)
      echo '{{"protocol":1,"id":"r3","kind":"cancel_result","target_id":"r2","active":true}}'
      echo '{{"protocol":1,"id":"r2","kind":"canceled","reason":"canceled"}}'
      ;;
    *'"kind":"shutdown"'*) exit 0 ;;
  esac
done"#
        );
        let second_body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*)
      touch '{late_dispatch}'
      echo '{{"protocol":1,"id":"r3","kind":"started","provider":"faux","model":"faux-model","api":"faux","billing_path":"api","auth_source":"test"}}'
      echo '{{"protocol":1,"id":"r3","kind":"result","tool_call":{{"name":"submit","arguments":{{"ok":true}}}},"stop_reason":"toolUse","usage":{{"input_tokens":1,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"total_tokens":3,"cost_total":0}}}}'
      ;;
    *'"kind":"shutdown"'*) exit 0 ;;
  esac
done"#,
            late_dispatch = late_dispatch.display(),
        );
        let (first_node, first_script) = write_fake_gateway(first_dir.path(), &first_body)?;
        let (second_node, second_script) = write_fake_gateway(second_dir.path(), &second_body)?;
        let mut first = ProcessGateway::spawn(&first_node, &first_script)?;
        let mut delayed = ProcessGateway::spawn(&second_node, &second_script)?;
        let first_control = first.control();
        register_interrupt_controls(vec![first_control.clone(), delayed.control()])?;

        // One ticket represents the whole batch. The delayed worker keeps this
        // pre-interrupt generation even after the first worker clears pending.
        let admission = DispatchAdmission::capture()?;
        let first_request = complete_request();
        let delayed_request = complete_request();
        let (release_delayed, wait_for_release) = mpsc::channel();
        let (first_result, delayed_result) = thread::scope(|scope| -> anyhow::Result<_> {
            let first_admission = admission;
            let delayed_admission = admission;
            let first_worker = &mut first;
            let first_request = &first_request;
            let first_handle = scope.spawn(move || {
                first_worker.complete_with_admission(
                    first_request,
                    Duration::from_secs(2),
                    first_admission,
                )
            });
            let delayed_worker = &mut delayed;
            let delayed_request = &delayed_request;
            let delayed_handle = scope.spawn(move || {
                wait_for_release
                    .recv()
                    .expect("delayed worker release sender");
                delayed_worker.complete_with_admission(
                    delayed_request,
                    Duration::from_secs(2),
                    delayed_admission,
                )
            });

            wait_until_active(&first_control)?;
            assert!(request_interrupt_cancellation());
            let first_result = first_handle.join().expect("first gateway worker");
            assert!(
                !INTERRUPT_PENDING.load(std::sync::atomic::Ordering::SeqCst),
                "the completed active worker should clear the pending bit"
            );
            release_delayed
                .send(())
                .expect("delayed worker release receiver");
            let delayed_result = delayed_handle.join().expect("delayed gateway worker");
            Ok((first_result, delayed_result))
        })?;

        assert!(
            matches!(first_result, Err(GatewayError::Canceled(_))),
            "got {first_result:?}"
        );
        assert!(
            matches!(delayed_result, Err(GatewayError::Canceled(_))),
            "got {delayed_result:?}"
        );
        assert!(
            !late_dispatch.exists(),
            "the delayed worker sent a completion after Ctrl-C"
        );
        delayed.complete(&complete_request(), Duration::from_secs(2))?;
        assert!(
            late_dispatch.exists(),
            "a fresh post-cancellation admission should still dispatch"
        );
        Ok(())
    }

    #[test]
    fn idle_interrupt_requests_immediate_exit() -> anyhow::Result<()> {
        let _interrupt_test = INTERRUPT_TEST_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("interrupt test lock poisoned"))?;
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"shutdown"'*) exit 0 ;;
  esac
done"#
        );
        let gateway = spawn_with(&body)?;
        register_interrupt_controls(vec![gateway.control()])?;

        assert!(
            !request_interrupt_cancellation(),
            "an idle first interrupt must make the handler exit 130"
        );
        Ok(())
    }

    #[test]
    fn interrupt_handler_does_not_wait_for_gateway_stdin() -> anyhow::Result<()> {
        let _interrupt_test = INTERRUPT_TEST_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("interrupt test lock poisoned"))?;
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"shutdown"'*) exit 0 ;;
  esac
done"#
        );
        let gateway = spawn_with(&body)?;
        let control = gateway.control();
        register_interrupt_controls(vec![control.clone()])?;
        *control
            .active_request
            .lock()
            .map_err(|_| anyhow::anyhow!("gateway active-request lock poisoned"))? =
            Some("r2".to_string());

        let stdin = gateway
            .writer
            .stdin
            .lock()
            .map_err(|_| anyhow::anyhow!("gateway stdin lock poisoned"))?;
        let (finished, completion) = mpsc::channel();
        let canceler = thread::spawn(move || {
            let first = request_interrupt_cancellation();
            let second = request_interrupt_cancellation();
            finished
                .send((first, second))
                .expect("interrupt result receiver");
        });
        let result = completion.recv_timeout(Duration::from_secs(1));
        drop(stdin);
        canceler.join().expect("interrupt thread");

        assert_eq!(
            result?,
            (true, false),
            "the first interrupt should enqueue cancellation and the second should force exit"
        );
        Ok(())
    }

    #[test]
    fn remote_errors_carry_code_and_retryability() -> anyhow::Result<()> {
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*)
      echo '{{"protocol":1,"id":"r2","kind":"error","error":{{"code":"capacity","message":"rate limited","retryable":true,"capacity":true}}}}'
      ;;
  esac
done"#
        );
        let mut gateway = spawn_with(&body)?;
        let error = gateway
            .complete(&complete_request(), Duration::from_secs(5))
            .expect_err("remote error");
        let GatewayError::Remote(remote) = error else {
            panic!("expected remote error, got {error:?}");
        };
        assert_eq!(remote.code, "capacity");
        assert!(remote.retryable && remote.capacity);
        Ok(())
    }

    #[test]
    fn registered_interrupt_control_cancels_while_completion_waits() -> anyhow::Result<()> {
        let _interrupt_test = INTERRUPT_TEST_LOCK
            .lock()
            .map_err(|_| anyhow::anyhow!("interrupt test lock poisoned"))?;
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*)
      echo '{{"protocol":1,"id":"r2","kind":"started","provider":"faux","model":"faux-model","api":"faux","billing_path":"api","auth_source":"test"}}'
      ;;
    *'"kind":"cancel"'*)
      echo '{{"protocol":1,"id":"r3","kind":"cancel_result","target_id":"r2","active":true}}'
      echo '{{"protocol":1,"id":"r2","kind":"canceled","reason":"canceled"}}'
      ;;
    *'"kind":"shutdown"'*) exit 0 ;;
  esac
done"#
        );
        let mut gateway = spawn_with(&body)?;
        register_interrupt_controls(vec![gateway.control()])?;
        let canceler = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            request_interrupt_cancellation()
        });
        let error = gateway
            .complete(&complete_request(), Duration::from_secs(5))
            .expect_err("completion should be canceled");
        assert!(matches!(error, GatewayError::Canceled(_)), "got {error:?}");
        assert!(canceler.join().expect("cancel thread"));
        assert!(!gateway.poisoned());
        Ok(())
    }

    #[test]
    fn mismatched_active_cancel_acknowledgement_fails_closed() -> anyhow::Result<()> {
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*)
      echo '{{"protocol":1,"id":"r2","kind":"started","provider":"faux","model":"faux-model","api":"faux","billing_path":"api","auth_source":"test"}}'
      ;;
    *'"kind":"cancel"'*)
      echo '{{"protocol":1,"id":"r3","kind":"cancel_result","target_id":"wrong","active":true}}'
      ;;
  esac
done"#
        );
        let mut gateway = spawn_with(&body)?;
        let control = gateway.control();
        let canceler = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            control.cancel_active()
        });
        let error = gateway
            .complete(&complete_request(), Duration::from_secs(5))
            .expect_err("mismatched active cancellation should fail");
        assert!(matches!(error, GatewayError::Protocol(_)), "got {error:?}");
        assert!(gateway.poisoned());
        assert!(canceler.join().expect("cancel thread")?);
        Ok(())
    }

    #[test]
    fn child_death_mid_request_fails_the_request() -> anyhow::Result<()> {
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*) exit 7 ;;
  esac
done"#
        );
        let mut gateway = spawn_with(&body)?;
        let error = gateway
            .complete(&complete_request(), Duration::from_secs(5))
            .expect_err("child exit");
        assert!(
            matches!(error, GatewayError::ChildExited(_)),
            "got {error:?}"
        );
        assert!(gateway.poisoned());
        Ok(())
    }

    #[test]
    fn malformed_frames_and_stalls_fail_closed() -> anyhow::Result<()> {
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*) echo 'not json at all' ;;
  esac
done"#
        );
        let mut gateway = spawn_with(&body)?;
        let error = gateway
            .complete(&complete_request(), Duration::from_secs(5))
            .expect_err("malformed frame");
        assert!(matches!(error, GatewayError::Protocol(_)), "got {error:?}");
        assert!(gateway.poisoned());

        // A gateway that accepts the request but never answers trips the
        // client-side timeout and poisons the connection.
        let body = format!(
            r#"while IFS= read -r line; do
  case "$line" in
    *'"kind":"hello"'*) echo '{READY}' ;;
    *'"kind":"complete"'*) sleep 60 ;;
  esac
done"#
        );
        let mut gateway = spawn_with(&body)?;
        let error = gateway
            .complete_with_grace(
                &complete_request(),
                Duration::from_millis(50),
                Duration::from_millis(50),
            )
            .expect_err("stall");
        assert!(matches!(error, GatewayError::Timeout(_)), "got {error:?}");
        assert!(gateway.poisoned());
        Ok(())
    }
}
