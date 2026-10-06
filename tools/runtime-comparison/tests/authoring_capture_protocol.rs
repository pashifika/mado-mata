//! These commands exercise only private-protocol refusal. No valid discovery
//! policy reaches an engine and no native target or permission is consulted.
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Owned(Child);
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn invoke(bytes: &[u8]) -> (std::process::ExitStatus, Vec<u8>, Vec<u8>) {
    let mut child = Owned(
        Command::new(env!("CARGO_BIN_EXE_mado-runtime-comparison"))
            .arg("authoring-capture-child")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut stdout = child.0.stdout.take().unwrap();
    let mut stderr = child.0.stderr.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut err = Vec::new();
        stdout.read_to_end(&mut out).unwrap();
        stderr.read_to_end(&mut err).unwrap();
        (out, err)
    });
    if let Some(mut input) = child.0.stdin.take() {
        // A non-engine child may already have emitted its typed refusal and exited.
        let _ = input.write_all(bytes);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some(exit) = child.0.try_wait().unwrap() {
            break exit;
        }
        assert!(
            Instant::now() < deadline,
            "private invocation/control EOF must settle without native work"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    let (out, err) = reader.join().unwrap();
    (exit, out, err)
}

#[cfg(feature = "engine")]
#[test]
fn malformed_invocation_and_control_eof_exit_before_native_discovery() {
    for bytes in [
        Vec::new(),
        u32::MAX.to_le_bytes().to_vec(),
        vec![2, 0, 0, 0, b'{', b'}'],
    ] {
        let (exit, stdout, stderr) = invoke(&bytes);
        assert!(!exit.success());
        assert!(
            stdout.is_empty(),
            "malformed invocation cannot advertise candidates or pixels"
        );
        assert!(String::from_utf8_lossy(&stderr).contains("CaptureTransport"));
    }
}

#[cfg(not(feature = "engine"))]
#[test]
fn non_engine_cli_has_a_typed_private_refusal_not_a_mock_native_path() {
    let (exit, bytes, stderr) = invoke(&[]);
    assert!(exit.success());
    assert!(stderr.is_empty());
    let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
    assert_eq!(length + 4, bytes.len());
    let event: serde_json::Value = serde_json::from_slice(&bytes[4..]).unwrap();
    assert_eq!(event["event"], "terminal");
    assert_eq!(event["primary"]["category"], "EngineUnavailable");
    assert_eq!(event["clean"], true);
}
