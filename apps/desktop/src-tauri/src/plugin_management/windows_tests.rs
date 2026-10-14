//! Native Windows process, ownership and target regressions. All mutable files
//! live below the checkout's .cache; no installed OMP registry is used here.

use super::process::{self, Client, Invocation, Limits, Operation, Output, RunError};
use super::target::{self, Environment};
use super::{OutcomeStatus, PluginAction, PluginManager, PluginState, windows_target};
use serde_json::json;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Write;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;

const MODE: &str = "MADO_OMP_WINDOWS_FIXTURE";
const MARKERS: &str = "MADO_OMP_WINDOWS_MARKERS";
const ARGUMENT: &str = "MADO_OMP_WINDOWS_ARGUMENT";
const ARGUMENTS: [&str; 3] = [
    "--exact",
    "plugin_management::windows_tests::process_fixture",
    "--nocapture",
];
const LIMITS: Limits = Limits {
    duration: Duration::from_secs(30),
    stdout: 1024 * 1024,
    stderr: 4096,
};

struct Root(PathBuf);

impl Root {
    fn new() -> Self {
        let cache = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../.cache");
        fs::create_dir_all(&cache).unwrap();
        let root = cache.join(format!("omp-windows-{}", crate::storage::new_id().unwrap()));
        fs::create_dir(&root).unwrap();
        Self(root.canonicalize().unwrap())
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn appears(path: &Path, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

fn fixture_command(mode: &str, markers: &Path) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(ARGUMENTS)
        .env(MODE, mode)
        .env(MARKERS, markers)
        .stdin(Stdio::null());
    command
}

fn delayed_marker(mode: &str, markers: &Path) {
    fs::write(markers.join(format!("{mode}.started")), b"").unwrap();
    if appears(&markers.join("released"), Duration::from_secs(20)) {
        fs::write(markers.join(format!("{mode}.late")), b"").unwrap();
    }
}

/// Runs only when explicitly relaunched by a test, and exercises real native
/// process creation. Descendants try to escape immediately, before the leader
/// performs its output/deadline behavior.
#[test]
#[expect(
    unsafe_code,
    reason = "test child controls inheritance of its own standard handles"
)]
fn process_fixture() {
    let (Ok(mode), Some(markers)) = (std::env::var(MODE), std::env::var_os(MARKERS)) else {
        return;
    };
    let markers = PathBuf::from(markers);
    // Null-stdio descendants must not retain the leader's other standard handles.
    // The hold scenario deliberately keeps them inheritable.
    if mode != "hold" {
        for handle in [
            std::io::stdout().as_raw_handle(),
            std::io::stderr().as_raw_handle(),
        ] {
            // SAFETY: These live standard handles belong to this explicit test
            // child; clearing inheritance neither closes nor changes their I/O.
            assert_ne!(
                unsafe {
                    windows_sys::Win32::Foundation::SetHandleInformation(
                        handle,
                        windows_sys::Win32::Foundation::HANDLE_FLAG_INHERIT,
                        0,
                    )
                },
                0
            );
        }
    }
    match mode.as_str() {
        "lock" => {
            let result = target::lease(&markers);
            fs::write(
                markers.join("lock.result"),
                match result {
                    Ok(_) => "owned".to_owned(),
                    Err(fault) => fault.category,
                },
            )
            .unwrap();
            return;
        }
        "arguments" => {
            assert_eq!(std::env::args_os().last(), std::env::var_os(ARGUMENT));
            println!("argument vector preserved");
            return;
        }
        "escape" => {
            fs::write(markers.join("escape.executed"), b"").unwrap();
            return;
        }
        "unrelated" | "grandchild" => {
            delayed_marker(&mode, &markers);
            return;
        }
        "descendant" => {
            fixture_command("grandchild", &markers)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            assert!(appears(
                &markers.join("grandchild.started"),
                Duration::from_secs(10)
            ));
            delayed_marker(&mode, &markers);
            return;
        }
        _ => {}
    }
    let mut escape = fixture_command("escape", &markers);
    escape
        .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match escape.spawn() {
        Err(_) => fs::write(markers.join("escape.refused"), b"").unwrap(),
        Ok(mut child) => {
            let _ = child.wait();
            panic!("an owned process escaped its Job");
        }
    }
    let mut descendant = fixture_command("descendant", &markers);
    if mode != "hold" {
        descendant.stdout(Stdio::null()).stderr(Stdio::null());
    }
    descendant.spawn().unwrap();
    assert!(appears(
        &markers.join("descendant.started"),
        Duration::from_secs(10)
    ));
    match mode.as_str() {
        "sleep" => std::thread::sleep(Duration::from_secs(20)),
        "stdout" => {
            let mut stdout = std::io::stdout().lock();
            while stdout.write_all(&[b'x'; 8192]).is_ok() {}
        }
        "stderr" => {
            let mut stderr = std::io::stderr().lock();
            for _ in 0..256 {
                stderr.write_all(&[b'x'; 8192]).unwrap();
            }
        }
        "failure" => std::process::exit(7),
        "exit" | "hold" => {}
        other => panic!("unexpected Windows process fixture {other}"),
    }
}

fn fixture_run(
    mode: &str,
    markers: &Path,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Output, RunError> {
    let program = std::env::current_exe().unwrap();
    let mut environment = Environment::capture(Ok(markers.to_path_buf())).child(markers, &program);
    environment.extend([(MODE, mode.into()), (MARKERS, markers.into())]);
    process::run(
        &program,
        &ARGUMENTS.map(OsStr::new),
        markers,
        &environment,
        limits,
        cancel,
    )
}

fn started(result: Result<Output, RunError>) -> String {
    match result {
        Err(RunError::Started(fault)) => fault.category,
        Err(error) => panic!("expected a started, settled command: {:?}", error.fault()),
        Ok(_) => panic!("expected a bounded interruption"),
    }
}

struct Unrelated {
    child: Child,
    root: Root,
}

impl Unrelated {
    fn start() -> Self {
        let root = Root::new();
        let child = fixture_command("unrelated", &root.0)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        assert!(appears(
            &root.0.join("unrelated.started"),
            Duration::from_secs(10)
        ));
        Self { child, root }
    }

    fn assert_settled(mut self, owned: &[&Root]) {
        for root in owned.iter().copied().chain([&self.root]) {
            fs::write(root.0.join("released"), b"").unwrap();
        }
        assert!(appears(
            &self.root.0.join("unrelated.late"),
            Duration::from_secs(10)
        ));
        assert!(self.child.wait().unwrap().success());
        std::thread::sleep(Duration::from_millis(300));
        for root in owned {
            assert!(root.0.join("escape.refused").exists());
            assert!(!root.0.join("escape.executed").exists());
            for mode in ["descendant", "grandchild"] {
                assert!(root.0.join(format!("{mode}.started")).exists());
                assert!(
                    !root.0.join(format!("{mode}.late")).exists(),
                    "owned process outlived settlement"
                );
            }
        }
    }
}

impl Drop for Unrelated {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn timeout_and_cancel_settle_the_native_tree_without_touching_other_processes() {
    let unrelated = Unrelated::start();
    let timeout = Root::new();
    let begun = Instant::now();
    // The deadline starts at process creation, not at fixture readiness. Under
    // a parallel suite it may expire before user code runs. Exercise that bound
    // with one leader; the cancellation case waits for the complete tree before
    // interrupting it, so containment cannot pass with unstarted descendants.
    assert_eq!(
        started(fixture_run(
            "unrelated",
            &timeout.0,
            Limits {
                duration: Duration::from_secs(5),
                ..LIMITS
            },
            &AtomicBool::new(false)
        )),
        "PluginTimeout"
    );
    assert!(begun.elapsed() < Duration::from_secs(10));
    fs::write(timeout.0.join("released"), b"").unwrap();
    let cancelled = Root::new();
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let cancelling = scope.spawn(|| {
            assert!(appears(
                &cancelled.0.join("descendant.started"),
                Duration::from_secs(10)
            ));
            cancel.store(true, Ordering::Release);
            Instant::now()
        });
        assert_eq!(
            started(fixture_run("sleep", &cancelled.0, LIMITS, &cancel)),
            "PluginInterrupted"
        );
        assert!(cancelling.join().unwrap().elapsed() < Duration::from_secs(5));
    });
    unrelated.assert_settled(&[&cancelled]);
    assert!(!timeout.0.join("unrelated.late").exists());
}

#[test]
fn exit_failure_and_output_limits_always_settle_descendants() {
    let unrelated = Unrelated::start();
    let normal = Root::new();
    let failure = Root::new();
    let stdout = Root::new();
    let stderr = Root::new();
    let held = Root::new();
    let done = fixture_run("exit", &normal.0, LIMITS, &AtomicBool::new(false))
        .unwrap_or_else(|error| panic!("{:?}", error.fault()));
    assert!(done.success);
    let failed = fixture_run("failure", &failure.0, LIMITS, &AtomicBool::new(false))
        .unwrap_or_else(|error| panic!("{:?}", error.fault()));
    assert!(!failed.success);
    assert_eq!(failed.code, Some(7));
    assert_eq!(
        started(fixture_run(
            "stdout",
            &stdout.0,
            Limits {
                stdout: 128,
                ..LIMITS
            },
            &AtomicBool::new(false)
        )),
        "PluginOutputLimit"
    );
    let truncated = fixture_run("stderr", &stderr.0, LIMITS, &AtomicBool::new(false))
        .unwrap_or_else(|error| panic!("{:?}", error.fault()));
    assert!(truncated.success);
    assert_eq!(truncated.stderr.len(), LIMITS.stderr);
    assert_eq!(
        started(fixture_run(
            "hold",
            &held.0,
            LIMITS,
            &AtomicBool::new(false)
        )),
        "PluginProcess"
    );
    unrelated.assert_settled(&[&normal, &failure, &stdout, &stderr, &held]);
}

#[test]
fn native_argument_quoting_preserves_untrusted_path_characters_without_a_shell() {
    let root = Root::new();
    let program = std::env::current_exe().unwrap();
    let argument = OsStr::new("space 日本語 \\\"quoted\\\" & %PATH% | > marker \\");
    let mut environment = Environment::capture(Ok(root.0.clone())).child(&root.0, &program);
    environment.extend([
        (MODE, "arguments".into()),
        (MARKERS, root.0.clone().into()),
        (ARGUMENT, argument.to_os_string()),
    ]);
    let mut arguments: Vec<&OsStr> = ARGUMENTS
        .iter()
        .map(|argument| OsStr::new(argument))
        .collect();
    arguments.extend([OsStr::new("--skip"), argument]);
    let output = process::run(
        &program,
        &arguments,
        &root.0,
        &environment,
        LIMITS,
        &AtomicBool::new(false),
    )
    .unwrap_or_else(|error| panic!("{:?}", error.fault()));
    assert!(
        output.success,
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(!root.0.join("marker").exists());
    assert!(matches!(
        fixture_run("exit", &root.0, LIMITS, &AtomicBool::new(true)),
        Err(RunError::NotStarted(_))
    ));
    assert!(!root.0.join("descendant.started").exists());
    assert!(matches!(
        process::run(
            &root.0.join("missing.exe"),
            &[],
            &root.0,
            &environment,
            LIMITS,
            &AtomicBool::new(false)
        ),
        Err(RunError::NotStarted(_))
    ));
}

#[test]
fn installation_lock_is_cross_process_alias_safe_and_not_replaceable() {
    let root = Root::new();
    fs::create_dir(root.0.join("alias")).unwrap();
    let alias = root.0.join("alias/..");
    let lease = target::lease(&root.0).unwrap();
    assert!(
        matches!(target::lease(&alias), Err(fault) if fault.category == "PluginInstallationBusy")
    );
    assert!(fs::remove_file(root.0.join(".omp/mado-mata-plugin.lock")).is_err());
    let mut child = fixture_command("lock", &alias)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(root.0.join("lock.result")).unwrap(),
        "PluginInstallationBusy"
    );
    target::fence(&root.0, &json!({"operation": "test"})).unwrap();
    drop(lease);
    let second = target::lease(&alias).unwrap();
    assert_eq!(
        target::unsettled(&alias).unwrap().category,
        "PluginUnsettled"
    );
    target::lift(&alias);
    assert!(target::unsettled(&root.0).is_none());
    drop(second);
    let mut child = fixture_command("lock", &alias)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(root.0.join("lock.result")).unwrap(),
        "owned"
    );
}

#[test]
fn an_existing_unprotected_lock_is_refused_without_repair_or_replacement() {
    let root = Root::new();
    fs::create_dir(root.0.join(".omp")).unwrap();
    let path = root.0.join(".omp/mado-mata-plugin.lock");
    fs::write(&path, b"preserved").unwrap();
    assert!(
        matches!(target::lease(&root.0), Err(fault) if fault.category == "PluginInstallationLock")
    );
    assert_eq!(fs::read(path).unwrap(), b"preserved");
}

#[test]
fn native_discovery_refuses_ambiguity_scripts_and_same_metadata_replacement() {
    let root = Root::new();
    let first = root.0.join("first");
    let second = root.0.join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    // These are only file-identity fixtures; they are never executed as OMP.
    let executable = first.join("omp.exe");
    fs::write(&executable, b"identity fixture").unwrap();
    fs::write(
        first.join("omp.cmd"),
        b"must not launch a command interpreter",
    )
    .unwrap();
    fs::write(first.join("omp"), b"must not launch a POSIX wrapper").unwrap();
    assert!(target::explicit(first.join("omp.cmd").to_str().unwrap()).is_err());
    let observed = target::discover(&[first.clone()]).unwrap();
    let hardlink = second.join("omp.exe");
    fs::hard_link(&executable, &hardlink).unwrap();
    assert!(target::discover(&[first.clone(), second.clone()]).is_ok());
    fs::remove_file(&hardlink).unwrap();
    fs::write(&hardlink, b"identity fixture").unwrap();
    assert!(
        matches!(target::discover(&[first.clone(), second]), Err((PluginState::Unsupported, fault)) if fault.category == "PluginTargetAmbiguous")
    );
    let old_time = fs::metadata(&executable).unwrap().modified().unwrap();
    fs::rename(&executable, first.join("prior.exe")).unwrap();
    fs::write(&executable, b"identity fixture").unwrap();
    File::options()
        .write(true)
        .open(&executable)
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    assert_ne!(observed, target::selected(&executable).unwrap());
}

#[test]
fn windows_environment_keeps_one_home_and_rejects_profile_and_dotenv_redirection() {
    let root = Root::new();
    let executable = root.0.join("bin/omp.exe");
    let environment = Environment {
        home: Ok(root.0.clone()),
        search: vec![root.0.join("bin")],
        path: Some(
            std::env::join_paths([PathBuf::from("relative"), root.0.join("tools")]).unwrap(),
        ),
        tmpdir: None,
        variables: vec![
            ("OMP_PROFILE", "work".into()),
            ("OMP_CODING_AGENT_DIR", root.0.join("other").into()),
        ],
        native: vec![("SYSTEMROOT", OsString::from("C:\\Windows"))],
    };
    assert_eq!(
        environment.overrides(&root.0),
        ["OMP_PROFILE", "OMP_CODING_AGENT_DIR"]
    );
    let child = environment.child(&root.0, &executable);
    for key in ["HOME", "USERPROFILE"] {
        assert_eq!(
            child.iter().find(|(name, _)| *name == key).unwrap().1,
            root.0.as_os_str()
        );
    }
    let path = &child.iter().find(|(name, _)| *name == "PATH").unwrap().1;
    assert!(std::env::split_paths(path).all(|path| path.is_absolute()));
    assert!(
        !child
            .iter()
            .any(|(name, _)| ["OMP_PROFILE", "OMP_CODING_AGENT_DIR"].contains(name))
    );
    fs::write(root.0.join(".env"), "omp_profile=work\n").unwrap();
    assert!(!target::dotenv(&root.0).unwrap().is_empty());
    fs::write(root.0.join(".env"), "OMP_PROFILE=default\n").unwrap();
    assert!(target::dotenv(&root.0).unwrap().is_empty());
}

#[test]
fn registration_identity_does_not_confuse_a_copy_with_a_path_alias() {
    let root = Root::new();
    let path = root.0.join("registration");
    fs::create_dir(&path).unwrap();
    let alias = root.0.join("./registration");
    assert!(windows_target::same_registration(
        alias.to_str().unwrap(),
        &path
    ));
    fs::create_dir(root.0.join("copy")).unwrap();
    assert!(!windows_target::same_registration(
        root.0.join("copy").to_str().unwrap(),
        &path
    ));
}

struct Observer(Arc<AtomicUsize>);

impl Client for Observer {
    fn run(
        &self,
        _invocation: &Invocation<'_>,
        operation: Operation<'_>,
    ) -> Result<Output, RunError> {
        let stdout = match operation {
            Operation::Version => b"omp/18.10.3".to_vec(),
            Operation::Help => b"link list uninstall --json".to_vec(),
            Operation::List => br#"{"npm":[],"marketplace":[]}"#.to_vec(),
            Operation::Link(_) | Operation::Uninstall => {
                self.0.fetch_add(1, Ordering::Relaxed);
                panic!("a stale observation must not dispatch a mutation");
            }
        };
        Ok(Output {
            success: true,
            code: Some(0),
            stdout,
            stderr: Vec::new(),
        })
    }
}

#[test]
fn replacing_the_reviewed_windows_file_refuses_the_action_even_with_identical_metadata() {
    let root = Root::new();
    let home = root.0.join("home");
    let data = root.0.join("data");
    let bin = root.0.join("bin");
    for directory in [&home, &data, &bin] {
        fs::create_dir(directory).unwrap();
    }
    let executable = bin.join("omp.exe");
    fs::write(&executable, b"identity fixture; never executed").unwrap();
    let environment = Environment {
        home: Ok(home),
        search: vec![bin],
        path: None,
        tmpdir: None,
        variables: Vec::new(),
        native: Vec::new(),
    };
    let mutations = Arc::new(AtomicUsize::new(0));
    let manager = PluginManager::with_client(
        Ok(data),
        environment,
        true,
        Box::new(Observer(mutations.clone())),
    );
    let observed = manager.inspect(None).unwrap();
    assert_eq!(observed.state, PluginState::Absent);
    let modified = fs::metadata(&executable).unwrap().modified().unwrap();
    fs::rename(&executable, executable.with_extension("old")).unwrap();
    fs::write(&executable, b"identity fixture; never executed").unwrap();
    File::options()
        .write(true)
        .open(&executable)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    let result = manager
        .apply(
            observed.observation.as_deref().unwrap(),
            PluginAction::Install,
        )
        .unwrap();
    let outcome = result.outcome.unwrap();
    assert_eq!(outcome.status, OutcomeStatus::Failed);
    assert_eq!(outcome.issue.unwrap().category, "PluginStale");
    assert_eq!(mutations.load(Ordering::Relaxed), 0);
}

#[test]
fn cli_link_path_refuses_a_different_directory_after_namespace_normalization() {
    let root = Root::new();
    let ordinary = root.0.join("payload");
    let distinct = root.0.join("payload.");
    fs::create_dir(&ordinary).unwrap();
    fs::create_dir(&distinct).unwrap();
    fs::write(ordinary.join("identity"), b"other directory").unwrap();
    fs::write(distinct.join("identity"), b"selected directory").unwrap();
    let canonical = distinct.canonicalize().unwrap();
    assert_eq!(
        fs::read(canonical.join("identity")).unwrap(),
        b"selected directory"
    );
    let fault = process::link_source(&canonical).unwrap_err();
    assert_eq!(fault.category, "PluginPayload");
}
