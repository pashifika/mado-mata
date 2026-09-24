use super::protocol::{Invocation, Operation, emit, emit_bytes, frame};
use crate::host::Host;
#[cfg(feature = "engine")]
use crate::model::Control;
use crate::model::{Fault, MAX_TRANSPORT_BYTES, encode_bounded};
use serde_json::{Value, json};
use std::io::BufReader;
use std::process::ChildStdout;
use std::sync::{
    Arc, Mutex, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc,
};
use std::thread;

static EXECUTION_EVIDENCE: OnceLock<mpsc::SyncSender<(&'static str, u64)>> = OnceLock::new();
static HOST_WAIT_REPORTED: AtomicBool = AtomicBool::new(false);
pub(super) static BACKEND_INITIALIZATION_STARTED: AtomicBool = AtomicBool::new(false);
static SCRIPT_LOGS: OnceLock<ScriptStream> = OnceLock::new();

struct ScriptStream {
    sender: mpsc::SyncSender<Value>,
    retained: Mutex<ScriptBuffer>,
    record_limit: usize,
    byte_limit: usize,
    run: String,
    attempt: u64,
}

#[derive(Default)]
struct ScriptBuffer {
    records: Vec<Value>,
    bytes: usize,
    dropped: u64,
}

/// Separate finite queues keep ordinary log pressure off lifecycle evidence.
#[derive(Clone)]
pub struct Observer {
    pub progress: mpsc::SyncSender<Value>,
    pub logs: mpsc::SyncSender<Value>,
    pub dropped_logs: Arc<AtomicU64>,
}

impl Observer {
    pub(super) fn progress(&self, value: &Value) {
        let mut progress = serde_json::Map::new();
        for key in [
            "event",
            "run",
            "attempt",
            "at_us",
            "reason",
            "supervisor_received_us",
            "operation",
            "stage",
        ] {
            if let Some(field) = value.get(key) {
                progress.insert(key.into(), field.clone());
            }
        }
        let _ = self.progress.try_send(Value::Object(progress));
    }

    fn log(&self, value: Value) {
        if self.logs.try_send(value).is_err() {
            self.dropped_logs.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) fn emit_script_log(at_us: u64, message: &str) {
    if let Some(stream) = SCRIPT_LOGS.get() {
        let mut retained = stream
            .retained
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if retained.records.len() >= stream.record_limit
            || message.len() > stream.byte_limit.saturating_sub(retained.bytes)
        {
            retained.dropped = retained.dropped.saturating_add(1);
            return;
        }
        let value = json!({
            "event":"ScriptLog", "source":"Script", "severity":"info",
            "run":stream.run, "attempt":stream.attempt,
            "sequence":retained.records.len() + 1, "at_us":at_us, "message":message
        });
        retained.bytes += message.len();
        retained.records.push(value.clone());
        let _ = stream.sender.try_send(value);
    }
}

pub(super) fn retain_script_logs(observations: &mut Value) {
    if let Some(stream) = SCRIPT_LOGS.get() {
        let retained = stream
            .retained
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        observations["script_logs"] = json!(retained.records);
        observations["script_logs_dropped"] = json!(retained.dropped);
    }
}

/// Called once by an armed language instruction/interrupt hook. An in-process
/// adapter has no child evidence channel; the hook never waits on stdout.
pub(crate) fn emit_vm_hook_reached(host: &Host) {
    if let Some(sender) = EXECUTION_EVIDENCE.get() {
        let _ = sender.try_send(("VmHookReached", host.control().elapsed_us()));
    }
}

/// The first host wait is a separate boundary from parser startup or VM entry.
/// Notify through the bounded channel, never block host work on the output pipe.
pub(crate) fn emit_host_wait_entered(host: &Host) {
    if let Some(sender) = EXECUTION_EVIDENCE.get()
        && !HOST_WAIT_REPORTED.swap(true, Ordering::Relaxed)
    {
        let _ = sender.try_send(("HostWaitEntered", host.control().elapsed_us()));
    }
}

#[cfg(feature = "engine")]
pub(crate) fn emit_backend_initialization_started(control: &Control) {
    BACKEND_INITIALIZATION_STARTED.store(true, Ordering::Release);
    if let Some(sender) = EXECUTION_EVIDENCE.get() {
        let _ = sender.try_send(("BackendInitializationStarted", control.elapsed_us()));
    }
}

// Diagnostic detail is expendable; accepted actions, receipts, release
// obligations, postconditions and ownership facts are not.
fn compact_observations(value: &mut Value) {
    let Some(fields) = value.as_object_mut() else {
        return;
    };
    fields.remove("logs");
    fields.remove("script_logs");
    fields.remove("failure");
    if let Some(engine) = fields.get_mut("engine").and_then(Value::as_object_mut) {
        engine.remove("configuration");
    }
    if let Some(receipts) = fields.get_mut("receipts").and_then(Value::as_array_mut) {
        for receipt in receipts {
            compact_receipt_diagnostics(receipt);
        }
    }
    fields.insert("diagnostic_details_omitted".into(), json!(true));
}

fn compact_receipt_diagnostics(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            for (name, value) in fields {
                if matches!(name.as_str(), "error" | "fault") && value.is_object() {
                    value.as_object_mut().expect("object").retain(|key, _| {
                        matches!(key.as_str(), "category" | "status" | "native_cleanup")
                    });
                    value["diagnostic_details_omitted"] = json!(true);
                } else {
                    compact_receipt_diagnostics(value);
                }
            }
        }
        Value::Array(values) => {
            for value in values {
                compact_receipt_diagnostics(value);
            }
        }
        _ => {}
    }
}

pub(super) fn settlement_bytes(mut value: Value) -> Result<Vec<u8>, Fault> {
    let bytes = match encode_bounded(&value, MAX_TRANSPORT_BYTES - 1) {
        Ok(bytes) => bytes,
        Err(error) if error.category == "LimitExceeded" => {
            compact_observations(&mut value["observations"]);
            if let Some(runtime) = value["runtime"].as_object_mut() {
                runtime.remove("source_diagnostic");
            }
            if let Some(cleanup) = value.get_mut("cleanup") {
                compact_receipt_diagnostics(cleanup);
            }
            value["diagnostic_details_omitted"] = json!(true);
            encode_bounded(&value, MAX_TRANSPORT_BYTES - 1)?
        }
        Err(error) => return Err(error),
    };
    Ok(bytes)
}

pub(super) fn emit_settlement(value: Value) -> Result<(), Fault> {
    emit_bytes(&settlement_bytes(value)?)
}

pub(super) fn start_evidence(invocation: &Invocation) {
    let run = &invocation.run;
    let attempt = invocation.attempt;
    if invocation.observe_logs && invocation.operation == Operation::Run {
        let (sender, receiver) = mpsc::sync_channel::<Value>(invocation.plan.limits.log_records);
        let _ = SCRIPT_LOGS.set(ScriptStream {
            sender,
            retained: Mutex::new(ScriptBuffer::default()),
            record_limit: invocation.plan.limits.log_records,
            byte_limit: invocation.plan.limits.log_bytes,
            run: run.clone(),
            attempt,
        });
        thread::spawn(move || {
            for value in receiver {
                let _ = emit(&value);
            }
        });
    }
    {
        // SDK initialization, instruction hook, and host wait have independent
        // bounded notifications; Check never emits either VM/workflow event.
        let (sender, receiver) = mpsc::sync_channel(3);
        let _ = EXECUTION_EVIDENCE.set(sender);
        let event_run = run.clone();
        let operation = invocation.operation;
        thread::spawn(move || {
            for (event, at_us) in receiver.iter().take(3) {
                let stage =
                    (event == "BackendInitializationStarted").then_some("backend_initialization");
                let _ = emit(&json!({"event":event,"run":event_run,"attempt":attempt,
                    "operation":operation.name(),"stage":stage,"at_us":at_us}));
            }
        });
    }
}

pub(super) fn receive_evidence(
    stdout: ChildStdout,
    observer: Option<&Observer>,
    invocation: &Invocation,
) -> (mpsc::Receiver<Result<Value, Fault>>, thread::JoinHandle<()>) {
    let run = &invocation.run;
    let plan = &invocation.plan;
    // Logs bypass the independent sixteen-record lifecycle allowance.
    let (sender, receiver) = mpsc::sync_channel(17);
    let log_observer = observer.cloned();
    let event_run = run.clone();
    let log_limit = if observer.is_some() {
        plan.limits.log_records
    } else {
        0
    };
    let reader = thread::spawn(move || {
        let mut input = BufReader::new(stdout);
        let mut control_records = 0;
        let mut log_records = 0;
        for _ in 0..16 + log_limit {
            match frame(&mut input, MAX_TRANSPORT_BYTES) {
                Ok(Some(bytes)) => {
                    let result = serde_json::from_slice::<Value>(&bytes)
                        .map_err(|e| Fault::new("Transport", e.to_string()));
                    if let Ok(value) = &result
                        && value["event"] == "ScriptLog"
                        && value["run"] == event_run
                        && value["attempt"] == 1
                        && log_records < log_limit
                    {
                        log_records += 1;
                        if let Some(observer) = &log_observer {
                            observer.log(value.clone());
                        }
                        continue;
                    }
                    control_records += 1;
                    if control_records > 16 {
                        break;
                    }
                    if sender.try_send(result).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    let _ = sender.try_send(Err(error));
                    return;
                }
            }
        }
        let _ = sender.try_send(Err(Fault::new(
            "Transport",
            "child exceeded protocol record bound",
        )));
    });
    (receiver, reader)
}

#[cfg(test)]
mod tests;
