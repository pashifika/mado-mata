use super::{ChildStdio, Environment, OwnedChild};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Command;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows_sys::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
};

const FIXTURE: &str = "MADO_OWNED_CHILD_TEST_FIXTURE";
const CHILD_MARKER: &str = "OWNED_CHILD_PID=";
const WAIT: Duration = Duration::from_secs(10);

fn fixture(mode: &str, stdio: ChildStdio) -> OwnedChild {
    OwnedChild::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "owned_child::tests::child_process_fixture",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(FIXTURE, mode),
        stdio,
        Environment::Inherited,
    )
    .unwrap()
}

// This same test executable supplies real owned descendants; no shell or selected target is run.
#[test]
fn child_process_fixture() {
    let Ok(mode) = std::env::var(FIXTURE) else {
        return;
    };
    if mode == "environment" {
        argument_environment_contract();
        return;
    }
    if mode == "leaf" {
        loop {
            thread::sleep(Duration::from_secs(1));
        }
    }
    assert!(matches!(mode.as_str(), "tree" | "abrupt"));
    let descendant = fixture("leaf", ChildStdio::Null);
    println!("{CHILD_MARKER}{}", descendant.id());
    std::io::stdout().flush().unwrap();
    if mode == "abrupt" {
        let mut byte = [0];
        std::io::stdin().read_exact(&mut byte).unwrap();
        // Bypass Rust Drop: only OS closure of the non-inherited Job handle owns cleanup.
        std::process::exit(0);
    }
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

#[expect(
    unsafe_code,
    reason = "retaining the exact application-created descendant for containment evidence"
)]
fn descendant(parent: &mut OwnedChild) -> OwnedHandle {
    let stdout = parent.stdout.take().unwrap();
    let (send, receive) = mpsc::sync_channel(1);
    let reader = thread::spawn(move || {
        for line in BufReader::new(stdout.take(4096)).lines().take(16) {
            let Ok(line) = line else { break };
            if let Some((_, pid)) = line.split_once(CHILD_MARKER) {
                let _ = send.send(pid.trim().parse::<u32>());
                return;
            }
        }
    });
    let received = receive.recv_timeout(WAIT);
    if received.is_err() {
        let _ = parent.kill();
    }
    reader.join().unwrap();
    let pid = received
        .expect("owned fixture must report its descendant before the deadline")
        .unwrap();
    // SAFETY: this PID comes from our still-live child; retain the handle before allowing exit.
    let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    assert!(
        !handle.is_null(),
        "owned descendant must remain alive until observed"
    );
    // SAFETY: OpenProcess returned one uniquely owned real handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(handle) };
    assert_eq!(
        unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) },
        WAIT_TIMEOUT
    );
    handle
}

fn bounded_wait(child: &mut OwnedChild) -> std::process::ExitStatus {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "owned child must settle before the deadline"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[expect(
    unsafe_code,
    reason = "waiting on retained owned-descendant handles without reopening a PID"
)]
#[test]
fn killing_or_dropping_owner_reaps_descendants_but_not_unrelated_process() {
    let mut unrelated = fixture("leaf", ChildStdio::Null);
    for drop_owner in [false, true] {
        let mut parent = fixture("tree", ChildStdio::Protocol);
        let process = descendant(&mut parent);
        if drop_owner {
            drop(parent);
        } else {
            parent.kill().unwrap();
            assert!(!bounded_wait(&mut parent).success());
        }
        // SAFETY: the retained handle identifies the same descendant even after PID reuse.
        assert_eq!(
            unsafe { WaitForSingleObject(process.as_raw_handle(), 10_000) },
            WAIT_OBJECT_0
        );
        assert!(
            unrelated.try_wait().unwrap().is_none(),
            "unrelated owned process must survive"
        );
    }
    unrelated.kill().unwrap();
    bounded_wait(&mut unrelated);
}

#[expect(
    unsafe_code,
    reason = "observing Job cleanup after real parent exit bypasses Rust destructors"
)]
#[test]
fn abrupt_parent_loss_closes_job_and_reaps_descendant() {
    let mut unrelated = fixture("leaf", ChildStdio::Null);
    let mut parent = fixture("abrupt", ChildStdio::Protocol);
    let process = descendant(&mut parent);
    parent.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(bounded_wait(&mut parent).success());
    // SAFETY: the exact still-retained descendant handle remains valid throughout this wait.
    assert_eq!(
        unsafe { WaitForSingleObject(process.as_raw_handle(), 10_000) },
        WAIT_OBJECT_0
    );
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "unrelated owned process must survive"
    );
    unrelated.kill().unwrap();
    bounded_wait(&mut unrelated);
}

#[test]
fn arguments_and_environment_cross_the_real_process_boundary_without_shell_parsing() {
    let mut child = fixture("environment", ChildStdio::Protocol);
    let stdout = child.stdout.take().unwrap();
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(16 * 1024).read_to_end(&mut bytes).unwrap();
        bytes
    });
    let status = bounded_wait(&mut child);
    let output = reader.join().unwrap();
    assert!(status.success(), "{}", String::from_utf8_lossy(&output));
}

fn argument_environment_contract() {
    let node = crate::typescript::node_from_path().unwrap();
    let arguments = [
        "",
        "plain",
        "two words",
        "世界 & %PATH% ^ | < >",
        "quote\"inside",
        "slash\\\"quote",
        "trailing\\",
        "\\\\",
    ];
    for environment in [Environment::Inherited, Environment::Cleared] {
        let mut command = Command::new(&node);
        command.args(["-e", "process.stdout.write(JSON.stringify({args:process.argv.slice(1),path:process.env.PATH,removed:process.env.MADO_REMOVED,value:process.env.MADO_VALUE,inherited:process.env.MADO_OWNED_CHILD_TEST_FIXTURE}))", "--"])
            .args(arguments)
            .env("Path", "first")
            .env("PATH", "last")
            .env("MADO_VALUE", "雪 = value")
            .env("MADO_REMOVED", "discard")
            .env_remove("MADO_REMOVED");
        if let Some(root) = std::env::var_os("SystemRoot") {
            command.env("SystemRoot", root);
        }
        let mut child = OwnedChild::spawn(&mut command, ChildStdio::Piped, environment).unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = thread::spawn(move || {
            let mut bytes = Vec::new();
            stdout.take(4096).read_to_end(&mut bytes).unwrap();
            bytes
        });
        assert!(bounded_wait(&mut child).success());
        let output: serde_json::Value = serde_json::from_slice(&reader.join().unwrap()).unwrap();
        let mut expected = serde_json::json!({"args":arguments,"path":"last","value":"雪 = value"});
        if matches!(environment, Environment::Inherited) {
            expected["inherited"] = serde_json::json!("environment");
        }
        assert_eq!(output, expected);
    }
    let mut child = OwnedChild::spawn(
        Command::new(node).args(["-e", "process.exit(259)"]),
        ChildStdio::Null,
        Environment::Inherited,
    )
    .unwrap();
    assert_eq!(bounded_wait(&mut child).code(), Some(259));
}

#[test]
fn invalid_arguments_are_refused_before_any_child_starts() {
    for argument in ["embedded\0nul".to_owned(), "x".repeat(32_768)] {
        let error = match OwnedChild::spawn(
            Command::new(std::env::current_exe().unwrap()).arg(argument),
            ChildStdio::Null,
            Environment::Inherited,
        ) {
            Ok(_) => panic!("invalid process argument was admitted"),
            Err(error) => error,
        };
        assert_eq!(error.context["boundary"], "parameters");
        assert_eq!(error.context["child_started"], false);
        assert_eq!(error.context["cleanup"]["clean"], true);
    }
}
