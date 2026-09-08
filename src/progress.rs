//! CLI-only progress. Library/MCP calls stay silent unless main installs a reporter.
//! Work counters describe the current phase, never an invented overall percentage.

use std::io::{self, IsTerminal, Write};
use std::sync::{OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

static ACTIVE: OnceLock<mpsc::SyncSender<Message>> = OnceLock::new();
const DELAY: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Stage { label: String, total: Option<usize> },
    Advance(usize),
    Total(usize),
    Detail(String),
    RequestStarted,
    RequestFinished,
    Idle,
}

enum Message {
    Update(Event),
    Finish(bool),
}

fn emit(event: Event) {
    #[cfg(test)]
    CAPTURE.with(|events| {
        if let Some(events) = events.borrow_mut().as_mut() {
            events.push(event.clone());
        }
    });
    if let Some(sender) = ACTIVE.get() {
        let _ = sender.send(Message::Update(event));
    }
}

pub fn stage(label: impl Into<String>, total: Option<usize>) {
    emit(Event::Stage {
        label: label.into(),
        total,
    });
}

pub fn advance(amount: usize) {
    emit(Event::Advance(amount));
}

pub fn set_total(total: usize) {
    emit(Event::Total(total));
}

pub fn detail(label: impl Into<String>) {
    emit(Event::Detail(label.into()));
}

/// Stop heartbeat while a daemon is idle; the next stage resumes it.
pub fn idle() {
    emit(Event::Idle);
}

/// Counts dispatched requests, not validated/published scouting outcomes.
pub fn request() -> Request {
    emit(Event::RequestStarted);
    Request
}

pub struct Request;

impl Drop for Request {
    fn drop(&mut self) {
        emit(Event::RequestFinished);
    }
}

pub struct Reporter {
    sender: mpsc::SyncSender<Message>,
    worker: Option<JoinHandle<()>>,
}

impl Reporter {
    pub fn start(command: &'static str, immediate: bool) -> Option<Self> {
        // Bound memory under slow stderr consumers. Backpressure preserves
        // every counter delta, just as ordinary CLI stderr writes do.
        let (sender, receiver) = mpsc::sync_channel(256);
        let interval = Duration::from_secs(if io::stderr().is_terminal() { 1 } else { 5 });
        let worker = std::thread::Builder::new()
            .name("jscout-progress".into())
            .spawn(move || report(receiver, command, immediate, interval, &mut io::stderr()))
            .ok()?;
        if ACTIVE.set(sender.clone()).is_err() {
            let _ = sender.send(Message::Finish(false));
            let _ = worker.join();
            return None;
        }
        Some(Self {
            sender,
            worker: Some(worker),
        })
    }

    pub fn finish(mut self, success: bool) {
        self.stop(success);
    }

    fn stop(&mut self, success: bool) {
        if let Some(worker) = self.worker.take() {
            let _ = self.sender.send(Message::Finish(success));
            let _ = worker.join();
        }
    }
}

impl Drop for Reporter {
    fn drop(&mut self) {
        self.stop(false);
    }
}

struct State {
    label: String,
    done: usize,
    total: Option<usize>,
    detail: String,
    active_requests: usize,
    finished_requests: usize,
    idle: bool,
}

impl State {
    fn new() -> Self {
        Self {
            label: "loading configuration".into(),
            done: 0,
            total: None,
            detail: String::new(),
            active_requests: 0,
            finished_requests: 0,
            idle: false,
        }
    }

    fn update(&mut self, event: Event) {
        match event {
            Event::Stage { label, total } => {
                self.label = label;
                self.total = total;
                self.done = 0;
                self.idle = false;
                self.detail.clear();
            }
            Event::Advance(amount) => self.done = self.done.saturating_add(amount),
            Event::Total(total) => self.total = Some(total),
            Event::Detail(detail) => self.detail = detail,
            Event::RequestStarted => self.active_requests += 1,
            Event::RequestFinished => {
                self.active_requests = self.active_requests.saturating_sub(1);
                self.finished_requests += 1;
            }
            Event::Idle => self.idle = true,
        }
    }

    fn line(&self, command: &str, elapsed: Duration, status: &str) -> String {
        let count = self
            .total
            .map_or_else(String::new, |total| format!(" {}/{total}", self.done));
        let requests = if self.active_requests + self.finished_requests > 0 {
            format!(
                " requests_active={} requests_finished={}",
                self.active_requests, self.finished_requests
            )
        } else {
            String::new()
        };
        let detail = if self.detail.is_empty() {
            String::new()
        } else {
            format!(" {}", self.detail)
        };
        format!(
            "progress [{command}] {status}: {}{count}{requests}{detail} elapsed={:.1}s",
            self.label,
            elapsed.as_secs_f64()
        )
    }
}

fn report(
    receiver: mpsc::Receiver<Message>,
    command: &str,
    immediate: bool,
    interval: Duration,
    out: &mut impl Write,
) {
    let start = Instant::now();
    let mut state = State::new();
    let mut last_report = Duration::ZERO;
    let mut visible = immediate;
    let mut dirty = false;
    if immediate {
        let _ = writeln!(out, "{}", state.line(command, Duration::ZERO, "running"));
    }
    loop {
        // Check elapsed time even under a continuous stream of counter updates.
        let wait = if state.idle {
            interval
        } else if visible {
            interval.saturating_sub(start.elapsed().saturating_sub(last_report))
        } else {
            DELAY.saturating_sub(start.elapsed())
        };
        let message = receiver.recv_timeout(wait);
        let elapsed = start.elapsed();
        let phase_change = match &message {
            Ok(Message::Update(Event::Stage { .. })) => true,
            Ok(Message::Update(Event::Idle)) => !state.idle,
            _ => false,
        };
        if phase_change && dirty && visible {
            let _ = writeln!(out, "{}", state.line(command, elapsed, "running"));
        }
        match message {
            Ok(Message::Finish(success)) => {
                if visible || (!state.idle && elapsed >= DELAY) {
                    let _ = writeln!(
                        out,
                        "{}",
                        state.line(
                            command,
                            elapsed,
                            if success { "completed" } else { "failed" }
                        )
                    );
                }
                return;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Ok(Message::Update(event)) => {
                state.update(event);
                dirty = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if state.idle {
            // Avoid a zero-timeout spin after pausing a long-running daemon.
            last_report = elapsed;
            dirty = false;
            continue;
        }
        let count_complete = state
            .total
            .is_some_and(|total| total > 0 && state.done == total);
        if (visible || elapsed >= DELAY)
            && (phase_change
                || (dirty && count_complete)
                || elapsed.saturating_sub(last_report) >= if visible { interval } else { DELAY })
        {
            let _ = writeln!(out, "{}", state.line(command, elapsed, "running"));
            last_report = elapsed;
            visible = true;
            dirty = false;
        }
    }
}

#[cfg(test)]
thread_local! {
    static CAPTURE: std::cell::RefCell<Option<Vec<Event>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub fn capture<T>(operation: impl FnOnce() -> T) -> (T, Vec<Event>) {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            CAPTURE.with(|events| *events.borrow_mut() = None);
        }
    }
    CAPTURE.with(|events| {
        assert!(
            events.borrow_mut().replace(Vec::new()).is_none(),
            "nested progress capture"
        );
    });
    let _reset = Reset;
    let result = operation();
    let events = CAPTURE.with(|events| events.borrow_mut().take().expect("capture installed"));
    (result, events)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_reset_and_dynamic_totals_do_not_claim_overall_completion() {
        let mut state = State::new();
        state.update(Event::Stage {
            label: "subjects processed".into(),
            total: Some(2),
        });
        state.update(Event::Advance(2));
        state.update(Event::Total(4));
        assert_eq!(
            state.line("scout repository", Duration::from_secs(8), "running"),
            "progress [scout repository] running: subjects processed 2/4 elapsed=8.0s"
        );
        state.update(Event::Stage {
            label: "publishing".into(),
            total: None,
        });
        assert_eq!(state.done, 0);
        assert!(
            !state
                .line("scout repository", Duration::ZERO, "failed")
                .contains("completed")
        );
    }

    #[test]
    fn request_drop_counts_errors_as_finished_not_successful() {
        let (_, events) = capture(|| {
            let _request = request();
        });
        assert_eq!(events, [Event::RequestStarted, Event::RequestFinished]);
    }

    #[test]
    fn repeated_idle_notifications_do_not_emit_running_heartbeats() {
        let (tx, rx) = mpsc::channel();
        tx.send(Message::Update(Event::Stage {
            label: "generation 1".into(),
            total: Some(2),
        }))
        .unwrap();
        tx.send(Message::Update(Event::Advance(1))).unwrap();
        for _ in 0..4 {
            tx.send(Message::Update(Event::Idle)).unwrap();
        }
        tx.send(Message::Update(Event::Stage {
            label: "generation 2".into(),
            total: None,
        }))
        .unwrap();
        tx.send(Message::Finish(true)).unwrap();
        let mut out = Vec::new();
        report(rx, "watch", true, Duration::from_secs(5), &mut out);
        let output = String::from_utf8(out).unwrap();
        assert_eq!(output.matches("generation 1 1/2").count(), 1, "{output}");
        assert_eq!(
            output.matches("running: generation 2").count(),
            1,
            "{output}"
        );
    }

    #[test]
    fn quick_commands_are_silent_but_explicit_operations_finish_honestly() {
        for (immediate, success) in [(false, true), (true, true), (true, false)] {
            let (tx, rx) = mpsc::channel();
            tx.send(Message::Update(Event::Stage {
                label: "embedding".into(),
                total: Some(2),
            }))
            .unwrap();
            tx.send(Message::Update(Event::Advance(1))).unwrap();
            tx.send(Message::Finish(success)).unwrap();
            let mut out = Vec::new();
            report(
                rx,
                "docs embed",
                immediate,
                Duration::from_secs(5),
                &mut out,
            );
            let output = String::from_utf8(out).unwrap();
            if immediate {
                assert!(output.contains("embedding 1/2"));
                assert!(output.contains(if success { "completed:" } else { "failed:" }));
            } else {
                assert!(output.is_empty());
            }
        }
    }
}
