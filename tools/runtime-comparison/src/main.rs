use mado_runtime_comparison::{check, inventory, model, report, runner};

use model::{Fault, Plan};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::sync::mpsc;

fn execute() -> Result<bool, Fault> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [command] if command == "child" => runner::child(),
        [command] if command == "parent-probe" => runner::parent_probe(false),
        [command] if command == "parent-stop-probe" => runner::parent_probe(true),
        [command] if command == "target-probe" => {
            std::thread::sleep(std::time::Duration::from_secs(30));
            Ok(true)
        }
        [command] if command == "check" => {
            let result = check::run()?;
            let passed = result["check_passed"].as_bool() == Some(true);
            print_json(&result)?;
            Ok(passed)
        }
        [command, plan_path, package] if command == "run" => {
            let plan: Plan = runner::read_json(Path::new(plan_path), 65_536)?;
            plan.validate()?;
            let inventory = inventory::Inventory::capture(Path::new(package), &plan.limits)?;
            let result = runner::sample(&plan, &inventory)?;
            let passed = result.iter().all(|row| row.status == "PASS");
            print_json(&serde_json::json!({"version":1,"runs":result}))?;
            Ok(passed)
        }
        [command, plan_path, package, output] if command == "manual" => {
            manual(Path::new(plan_path), Path::new(package), Path::new(output))
        }
        [command, path] if command == "report" => {
            let data = runner::read_json(Path::new(path), model::MAX_TRANSPORT_BYTES)?;
            print_json(&report::summarize(&data)?)?;
            Ok(true)
        }
        [] => {
            help();
            Ok(true)
        }
        [arg] if arg == "--help" || arg == "-h" => {
            help();
            Ok(true)
        }
        _ => Err(Fault::new(
            "Arguments",
            "usage: mado-runtime-comparison check | run PLAN PACKAGE | manual PLAN PACKAGE OUTPUT | report RESULTS",
        )),
    }
}

fn help() {
    println!(
        "mado-runtime-comparison\n  check                       Run controlled VM, host, and process scenarios\n  run PLAN PACKAGE            Execute an immutable, bounded comparison plan\n  manual PLAN PACKAGE OUTPUT  Confirm one invocation; stop or control EOF requests cleanup\n  report RESULTS              Summarize evidence without inferring native adoption\nManual reserves a new private result file before confirmation; no warmups or repetition.\nNative operations require explicit finite authority; CI grants none."
    );
}

fn manual(plan_path: &Path, package: &Path, output_path: &Path) -> Result<bool, Fault> {
    let mut plan: Plan = runner::read_json(plan_path, 65_536)?;
    plan.validate()?;
    plan.samples = 1;
    plan.warmups = 0;
    plan.repetitions = 1;
    let inventory = inventory::Inventory::capture(package, &plan.limits)?;
    let mut output = reserve_manual_output(plan_path, package, output_path)?;
    eprintln!(
        "Manual: candidate={} lane={} scenario={} profile={}; exactly one invocation, no warmups or repetition.",
        plan.candidate, plan.lane, plan.scenario, plan.profile
    );
    eprintln!(
        "Bounds unchanged: duration={}ms cleanup={}ms containment={}ms max_actions={}.",
        plan.limits.duration_ms,
        plan.limits.cleanup_ms,
        plan.limits.containment_ms,
        plan.limits.max_actions
    );
    if plan.lane == "native" {
        eprintln!(
            "Native authority: {}. Starting is the operator's action, not authorization or qualification granted by this tool.",
            if plan.native_config.is_some() {
                "operator-supplied plan only; existing native checks still apply"
            } else {
                "absent; native work remains blocked"
            }
        );
    } else {
        eprintln!("Native authority: none granted by manual mode; the selected lane is unchanged.");
    }
    eprintln!(
        "Type exactly start then Enter to execute. Any other line or EOF cancels without invocation."
    );
    let mut input = BufReader::new(std::io::stdin());
    let confirmation = manual_command(&mut input);
    if !matches!(&confirmation, Ok(Some(command)) if command == b"start") {
        save_manual_output(
            &mut output,
            &serde_json::json!({
                "version":1,"runs":[],
                "manual":{"outcome":if confirmation.is_err() {"ControlFailedBeforeStart"} else {"CancelledBeforeStart"},
                    "invocations":0,"error":confirmation.as_ref().err()}
            }),
        )?;
        eprintln!(
            "No invocation: start was not confirmed; the reserved result contains no RunRecord."
        );
        confirmation?;
        return Ok(false);
    }
    eprintln!(
        "Starting once. Type exactly stop then Enter; control EOF also requests Stop. Waiting for owned cleanup/containment is mandatory."
    );
    let (send, receive) = mpsc::sync_channel(1);
    // Stdin may remain open after entry completion. Do not join this CLI-only
    // reader: main exits after the supervisor has reaped its owned child.
    std::thread::Builder::new()
        .name("manual-control".into())
        .spawn(move || {
            let request = loop {
                match manual_command(&mut input) {
                    Ok(Some(command)) if command == b"stop" => break Ok("Stop"),
                    Ok(Some(_)) => continue,
                    Ok(None) => break Ok("ControlEof"),
                    Err(error) => break Err(error),
                }
            };
            // Queue control before any terminal output can block this reader.
            if send.send(request).is_ok() {
                eprintln!("Stop requested; waiting for cleanup or bounded containment, not assuming physical cleanup.");
            }
        })
        .map_err(|error| Fault::new("Control", error.to_string()))?;
    let mut control = None;
    let result = {
        let mut poll = || match receive.try_recv() {
            Ok(request) => {
                control = Some(request);
                true
            }
            Err(mpsc::TryRecvError::Empty) => false,
            Err(mpsc::TryRecvError::Disconnected) => {
                control = Some(Err(Fault::new("Control", "operator control reader exited")));
                true
            }
        };
        runner::run_once(&plan, &inventory, None, false, Some(&mut poll))
    };
    let record = match result {
        Ok(record) => record,
        Err(error) => {
            save_manual_output(
                &mut output,
                &serde_json::json!({
                    "version":1,"runs":[],
                    "manual":{"outcome":"SupervisorFailed","invocations":null,"error":error,"control":control}
                }),
            )?;
            eprintln!(
                "Supervisor failed: no RunRecord; child entry and physical cleanup are not established."
            );
            return Err(error);
        }
    };
    let control_failed = control.as_ref().is_some_and(Result::is_err);
    let passed = record.status == "PASS" && !control_failed;
    let outcome = if control_failed {
        "ControlFailed"
    } else if record
        .primary
        .as_ref()
        .is_some_and(|fault| fault.category == "Cancelled")
    {
        "Cancelled"
    } else if record.status == "PASS" {
        "Completed"
    } else {
        "FailedOrBlocked"
    };
    save_manual_output(
        &mut output,
        &serde_json::json!({
            "version":1,"runs":[record],
            "manual":{"outcome":outcome,"invocations":1,"control":control}
        }),
    )?;
    eprintln!(
        "{outcome}: status={} entry={} primary={} cleanup={} forced={}; private result saved.",
        record.status,
        record.entry_outcome,
        record
            .primary
            .as_ref()
            .map_or("none", |fault| fault.category.as_str()),
        if record.cleanup["clean"] == true && !record.forced && record.exit_code == Some(0) {
            "settled"
        } else {
            "incomplete-or-unverified"
        },
        record.forced
    );
    eprintln!("Submitted does not prove effect; Stop alone does not prove physical cleanup.");
    Ok(passed)
}

fn manual_command(input: &mut impl BufRead) -> Result<Option<Vec<u8>>, Fault> {
    let mut bytes = Vec::new();
    input
        .take(1025)
        .read_until(b'\n', &mut bytes)
        .map_err(|error| Fault::new("Control", error.to_string()))?;
    if bytes.len() > 1024 {
        return Err(Fault::new("Control", "operator command exceeds 1024 bytes"));
    }
    if bytes.last() != Some(&b'\n') {
        // A partial line at EOF is not confirmation.
        return Ok(None);
    }
    bytes.pop();
    if bytes.last() == Some(&b'\r') {
        bytes.pop();
    }
    Ok(Some(bytes))
}

fn reserve_manual_output(plan: &Path, package: &Path, output: &Path) -> Result<File, Fault> {
    let filename = output
        .file_name()
        .ok_or_else(|| Fault::new("Output", "OUTPUT must name a new file"))?;
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let path = parent
        .canonicalize()
        .map_err(|error| Fault::new("Output", error.to_string()))?
        .join(filename);
    let plan = plan
        .canonicalize()
        .map_err(|error| Fault::new("Output", error.to_string()))?;
    let package = package
        .canonicalize()
        .map_err(|error| Fault::new("Output", error.to_string()))?;
    if path == plan || path.starts_with(package) {
        return Err(Fault::new(
            "Output",
            "OUTPUT must be separate from PLAN and outside PACKAGE",
        ));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // create_new refuses existing files, hard links, and symlinks atomically.
    // Keep this handle; never reopen or truncate an operator-supplied path.
    options
        .open(path)
        .map_err(|error| Fault::new("Output", error.to_string()))
}

fn save_manual_output(output: &mut File, value: &serde_json::Value) -> Result<(), Fault> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| Fault::new("Output", error.to_string()))?;
    bytes.push(b'\n');
    if bytes.len() > model::MAX_TRANSPORT_BYTES {
        return Err(Fault::new(
            "Output",
            "manual result exceeds the report input bound",
        ));
    }
    if let Err(error) = output.write_all(&bytes).and_then(|()| output.sync_all()) {
        // A failed save must not leave a valid-looking success document.
        let invalidation = output.set_len(0).and_then(|()| output.sync_all());
        return Err(Fault::new(
            "Output",
            format!("result save failed: {error}; OUTPUT is not valid evidence"),
        )
        .with_context(serde_json::json!({"cause":error.to_string(),
                "invalidation_error":invalidation.err().map(|error| error.to_string())})));
    }
    Ok(())
}

fn print_json(value: &serde_json::Value) -> Result<(), Fault> {
    use std::io::Write;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)
        .map_err(|e| Fault::new("Output", e.to_string()))?;
    writeln!(stdout).map_err(|e| Fault::new("Output", e.to_string()))
}

fn main() {
    let code = match execute() {
        Ok(true) => 0,
        Ok(false) => 1,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    };
    std::process::exit(code);
}
