//! Owned-filesystem coverage of the management Interface. The in-process client
//! registers real symlinks like OMP's user npm/link registry; it is not evidence
//! about an installed OMP, whose lifecycle is accepted separately.

use super::payload::{self, FILES, NAME, PREDECESSOR};
use super::process::{self, Client, Invocation, Limits, Operation, Output, RunError};
use super::target::{self, Environment, display};
use super::version::Version;
use super::{
    PluginAction, PluginInstallation, PluginManager, PluginState, PluginView, journal_path, lock,
};
use crate::configuration::tests::Root;
use crate::configuration::{create_private_directory, write_private};
use mado_runtime_comparison::model::Fault;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::OutcomeStatus::{Failed, Unknown, Verified};
use super::PluginAction::{Install, Migrate, Uninstall, Update};
use super::PluginState::{Absent, Conflict, Current, Incompatible, Legacy, Missing, Unsupported};

const UNRELATED: &str = "unrelated-plugin";
// Relevant output from OMP 18.8.7 and 18.8.9, not the internal CLI help template.
const HELP: &str = "Manage plugins (install, uninstall, list, etc.)
ARGUMENTS
  ACTION    Plugin action (install|uninstall|list|link|doctor|features|config|enable|disable|marketplace|discover|upgrade)
  TARGETS   Packages, paths, or plugin names
FLAGS
      --json             Output JSON
";

/// Scripted results for the next mutation; unscripted mutations apply normally.
/// A version probe scripted in `reads` is only held (`Hold`, `AwaitCancel`)
/// and then answers normally.
enum Behavior {
    /// Exits successfully without changing the registry.
    Ignore,
    /// Exits unsuccessfully without changing the registry.
    Refuse,
    /// Changes the registry, then exits unsuccessfully.
    FailAfter,
    /// Changes the registry, then the bounded run expires.
    Timeout,
    /// Changes the registry, then its command tree cannot be confirmed ended.
    Unsettled,
    Hold(mpsc::Sender<()>, mpsc::Receiver<()>),
    AwaitCancel(mpsc::Sender<()>),
}

struct Fake {
    version: Mutex<String>,
    help: Mutex<String>,
    marketplace: Mutex<Vec<Value>>,
    behaviors: Mutex<VecDeque<Behavior>>,
    reads: Mutex<VecDeque<Behavior>>,
    mutations: AtomicUsize,
    /// Every client run, including probes and inventory reads.
    runs: AtomicUsize,
    /// Whether each inventory read's command tree is left unsettled.
    unsettled_list: AtomicBool,
}

struct Shared(Arc<Fake>);

impl Client for Shared {
    fn run(
        &self,
        invocation: &Invocation<'_>,
        operation: Operation<'_>,
    ) -> Result<Output, RunError> {
        self.0.run(invocation, operation)
    }
}

fn exited(success: bool, stdout: Vec<u8>) -> Output {
    Output {
        success,
        code: Some(i32::from(!success)),
        stdout,
        stderr: if success {
            Vec::new()
        } else {
            b"refused".to_vec()
        },
    }
}

fn remove(path: &Path) {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path).unwrap(),
        Ok(_) => fs::remove_file(path).unwrap(),
        Err(_) => {}
    }
}

impl Fake {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            version: Mutex::new("omp/18.8.9\n".into()),
            help: Mutex::new(HELP.into()),
            marketplace: Mutex::new(Vec::new()),
            behaviors: Mutex::new(VecDeque::new()),
            reads: Mutex::new(VecDeque::new()),
            mutations: AtomicUsize::new(0),
            runs: AtomicUsize::new(0),
            unsettled_list: AtomicBool::new(false),
        })
    }

    fn mutations(&self) -> usize {
        self.mutations.load(Ordering::SeqCst)
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }

    fn script(&self, behavior: Behavior) {
        lock(&self.behaviors).push_back(behavior);
    }

    fn run(
        &self,
        invocation: &Invocation<'_>,
        operation: Operation<'_>,
    ) -> Result<Output, RunError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        assert!(
            matches!(target::lease(invocation.home), Err(fault) if fault.category == "PluginInstallationBusy"),
            "client work runs only while the user installation is owned"
        );
        let registry = invocation.home.join(".omp/plugins/node_modules");
        match operation {
            Operation::Version => {
                let read = lock(&self.reads).pop_front();
                match read {
                    None => {}
                    Some(Behavior::Hold(entered, release)) => {
                        entered.send(()).unwrap();
                        release.recv().unwrap();
                    }
                    Some(Behavior::AwaitCancel(entered)) => {
                        entered.send(()).unwrap();
                        await_cancel(invocation);
                    }
                    Some(_) => unreachable!("a read is only held"),
                }
                Ok(exited(true, lock(&self.version).clone().into_bytes()))
            }
            Operation::Help => Ok(exited(true, lock(&self.help).clone().into_bytes())),
            Operation::List => {
                if self.unsettled_list.load(Ordering::SeqCst) {
                    return Err(RunError::Unsettled(Fault::new(
                        "PluginUnsettled",
                        "fixture tree",
                    )));
                }
                Ok(exited(true, list(&registry, &lock(&self.marketplace))))
            }
            Operation::Link(_) | Operation::Uninstall => {
                self.mutations.fetch_add(1, Ordering::SeqCst);
                let behavior = lock(&self.behaviors).pop_front();
                match behavior {
                    None => Ok(mutate(&registry, operation)),
                    Some(Behavior::Ignore) => Ok(exited(true, Vec::new())),
                    Some(Behavior::Refuse) => Ok(exited(false, Vec::new())),
                    Some(Behavior::FailAfter) => {
                        mutate(&registry, operation);
                        Ok(exited(false, Vec::new()))
                    }
                    Some(Behavior::Timeout) => {
                        mutate(&registry, operation);
                        Err(RunError::Started(Fault::new(
                            "PluginTimeout",
                            "fixture deadline",
                        )))
                    }
                    Some(Behavior::Unsettled) => {
                        mutate(&registry, operation);
                        Err(RunError::Unsettled(Fault::new(
                            "PluginUnsettled",
                            "fixture tree",
                        )))
                    }
                    Some(Behavior::Hold(entered, release)) => {
                        entered.send(()).unwrap();
                        release.recv().unwrap();
                        Ok(mutate(&registry, operation))
                    }
                    Some(Behavior::AwaitCancel(entered)) => {
                        entered.send(()).unwrap();
                        await_cancel(invocation);
                        Err(RunError::Started(process::interrupted()))
                    }
                }
            }
        }
    }
}

fn await_cancel(invocation: &Invocation<'_>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !invocation.cancel.load(Ordering::Acquire) {
        assert!(
            Instant::now() < deadline,
            "shutdown must interrupt the child"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Lists readable registrations the way the user registry reports them.
fn list(registry: &Path, marketplace: &[Value]) -> Vec<u8> {
    let mut npm = Vec::new();
    for name in [NAME, UNRELATED] {
        let path = registry.join(name);
        let Ok(bytes) = fs::read(path.join("package.json")) else {
            continue;
        };
        let manifest: Value = serde_json::from_slice(&bytes).unwrap();
        npm.push(json!({
            "name": name,
            "version": manifest["version"],
            "path": path,
            "manifest": {"version": manifest["version"]},
            "enabledFeatures": null,
            "enabled": true,
        }));
    }
    serde_json::to_vec_pretty(&json!({"npm": npm, "marketplace": marketplace})).unwrap()
}

/// Relinking replaces only the registration; removal deletes only the registration.
fn mutate(registry: &Path, operation: Operation<'_>) -> Output {
    let registration = registry.join(NAME);
    if let Operation::Link(source) = operation {
        fs::create_dir_all(registration.parent().unwrap()).unwrap();
        remove(&registration);
        symlink(source, &registration).unwrap();
        let version = payload::included().unwrap().version.to_string();
        return exited(
            true,
            serde_json::to_vec(&json!({"name": NAME, "version": version, "path": source})).unwrap(),
        );
    }
    if fs::symlink_metadata(&registration).is_err() {
        return exited(false, Vec::new());
    }
    remove(&registration);
    exited(
        true,
        serde_json::to_vec(&json!({"uninstalled": NAME})).unwrap(),
    )
}

fn manifest(version: &str) -> Vec<u8> {
    format!(
        r#"{{"name":"{NAME}","version":"{version}","type":"module","omp":{{"extensions":["index.mjs"]}}}}"#
    )
    .into_bytes()
}

/// A target-resolution file; the in-process client never executes it.
fn executable(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"fixture executable; never run").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

fn snapshot(directory: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = fs::read_dir(directory)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                fs::read(entry.path()).unwrap(),
            )
        })
        .collect();
    files.sort();
    files
}

/// Only HOME and the fixture executable directory; no host variables.
fn environment(home: &Path, bin: &Path) -> Environment {
    Environment {
        home: Ok(home.to_path_buf()),
        search: vec![bin.to_path_buf()],
        path: None,
        tmpdir: None,
        variables: Vec::new(),
    }
}

/// Keeps a directory unwritable for its unprivileged owner until dropped.
struct ReadOnly(PathBuf, fs::Permissions);

impl ReadOnly {
    fn new(directory: &Path) -> Self {
        let restore = Self(
            directory.to_path_buf(),
            fs::metadata(directory).unwrap().permissions(),
        );
        fs::set_permissions(directory, fs::Permissions::from_mode(0o500)).unwrap();
        let probe = fs::write(directory.join("permission-probe"), b"");
        assert_eq!(
            probe.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "this regression requires an unprivileged filesystem owner"
        );
        restore
    }
}

impl Drop for ReadOnly {
    fn drop(&mut self) {
        fs::set_permissions(&self.0, self.1.clone()).unwrap();
    }
}

struct Fixture {
    _root: Root,
    base: PathBuf,
    data: PathBuf,
    home: PathBuf,
    bin: PathBuf,
    fake: Arc<Fake>,
    manager: Arc<PluginManager>,
}

impl Fixture {
    fn new() -> Self {
        Self::build(true, |_, _| {})
    }

    fn with(configure: impl FnOnce(&Path, &mut Environment)) -> Self {
        Self::build(true, configure)
    }

    fn build(platform: bool, configure: impl FnOnce(&Path, &mut Environment)) -> Self {
        let root = Root::new();
        let base = root.0.canonicalize().unwrap();
        let data = base.join("data");
        crate::storage::private_directory(&data).unwrap();
        let home = base.join("home");
        fs::create_dir_all(home.join(".omp/plugins/node_modules")).unwrap();
        let bin = base.join("bin");
        executable(&bin.join("omp"));
        let mut environment = environment(&home, &bin);
        configure(&base, &mut environment);
        let fake = Fake::new();
        let manager = Arc::new(PluginManager::with_client(
            Ok(data.clone()),
            environment,
            platform,
            Box::new(Shared(fake.clone())),
        ));
        Self {
            _root: root,
            base,
            data,
            home,
            bin,
            fake,
            manager,
        }
    }

    fn environment(&self) -> Environment {
        environment(&self.home, &self.bin)
    }

    /// Another application instance: its own data root and client, this HOME.
    fn peer(&self, name: &str) -> (PathBuf, Arc<Fake>, PluginManager) {
        let data = self.base.join(name);
        crate::storage::private_directory(&data).unwrap();
        let fake = Fake::new();
        let manager = PluginManager::with_client(
            Ok(data.clone()),
            self.environment(),
            true,
            Box::new(Shared(fake.clone())),
        );
        (data, fake, manager)
    }

    fn registry(&self) -> PathBuf {
        self.home.join(".omp/plugins/node_modules")
    }

    fn registration(&self) -> PathBuf {
        self.registry().join(NAME)
    }

    fn register(&self, source: &Path) {
        let registration = self.registration();
        fs::create_dir_all(registration.parent().unwrap()).unwrap();
        remove(&registration);
        symlink(source, &registration).unwrap();
    }

    /// Another registered plugin and its source, which management never touches.
    fn unrelated(&self) -> PathBuf {
        let source = self.base.join("unrelated");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("package.json"),
            br#"{"name":"unrelated-plugin","version":"2.0.0"}"#,
        )
        .unwrap();
        symlink(&source, self.registry().join(UNRELATED)).unwrap();
        source
    }

    fn checkout(&self, name: &str, version: &str, index: &[u8]) -> PathBuf {
        let source = self.base.join(name);
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("package.json"), manifest(version)).unwrap();
        fs::write(source.join("index.mjs"), index).unwrap();
        fs::write(source.join("client.mjs"), FILES[2].1).unwrap();
        source
    }

    fn predecessor(&self, name: &str) -> PathBuf {
        let source = self.checkout(
            name,
            PREDECESSOR,
            include_bytes!("../../fixtures/omp-predecessor/index.mjs"),
        );
        fs::write(
            source.join("client.mjs"),
            include_bytes!("../../fixtures/omp-predecessor/client.mjs"),
        )
        .unwrap();
        source
    }

    /// A private copy in the managed layout, as an earlier preparation leaves it.
    fn managed(&self, version: &str, index: &[u8]) -> PathBuf {
        let files = [
            ("package.json", manifest(version)),
            ("index.mjs", index.to_vec()),
            ("client.mjs", FILES[2].1.to_vec()),
        ];
        let content =
            payload::identity(files.iter().map(|(name, bytes)| (*name, bytes.as_slice())));
        let base = payload::base(&self.data);
        crate::storage::private_directory(&base).unwrap();
        let directory = base.join(format!("{version}-{content}"));
        create_private_directory(&directory).unwrap();
        for (name, bytes) in &files {
            write_private(&directory.join(name), bytes).unwrap();
        }
        directory
    }

    fn prepared(&self) -> PathBuf {
        let included = payload::included().unwrap();
        payload::base(&self.data).join(format!("{}-{}", included.version, included.content))
    }

    fn journal(&self) -> PathBuf {
        journal_path(&self.data)
    }

    fn inspect(&self) -> PluginView {
        self.manager.inspect(None).unwrap()
    }

    fn apply(&self, view: &PluginView, action: PluginAction) -> Result<PluginView, Fault> {
        self.manager
            .apply(view.observation.as_deref().unwrap_or("none"), action)
    }
}


#[test]
fn versions_follow_release_precedence() {
    let version = |text| Version::parse(text).unwrap();
    assert!(version("18.8.7-rc.1") < version("18.8.7"));
    assert!(version("18.8.8-rc.1") > version("18.8.7"));
    assert!(version("18.10.0") > version("18.9.9"));
    assert!(version("1.0.0-alpha.2") < version("1.0.0-alpha.10"));
    assert_eq!(version("0.1.1+build.7"), version("0.1.1"));
    for invalid in ["", "1.2", "1.2.3.4", "1.x.3", "1.2.3-", "v1.2.3"] {
        assert!(Version::parse(invalid).is_none(), "{invalid}");
    }
    assert_eq!(
        target::omp_version(b"omp/18.8.9\n").unwrap().to_string(),
        "18.8.9"
    );
    assert!(target::omp_version(b"18.8.9").is_none());
    assert!(target::omp_version(b"omp/18.8.9\nomp/18.8.10").is_none());
}

#[test]
fn profile_and_agent_state_overrides_are_refused_and_never_inherited() {
    let root = Root::new();
    let home = root.0.join("home");
    let data = root.0.join("xdg");
    fs::create_dir_all(data.join("omp")).unwrap();
    let environment = |variables: Vec<(&'static str, OsString)>| Environment {
        home: Ok(home.clone()),
        search: Vec::new(),
        path: Some("/usr/bin".into()),
        tmpdir: Some("/tmp/owned".into()),
        variables,
    };
    let allowed = environment(vec![
        ("OMP_PROFILE", "default".into()),
        ("PI_PROFILE", " ".into()),
        ("PI_CODING_AGENT_DIR", home.join(".omp/agent").into()),
        ("PI_CONFIG_DIR", ".omp".into()),
        ("XDG_DATA_HOME", root.0.join("empty").into()),
    ]);
    assert!(allowed.overrides(&home).is_empty());
    let redirected = environment(vec![
        ("OMP_PROFILE", "work".into()),
        ("PI_PROFILE", "work".into()),
        ("PI_CODING_AGENT_DIR", "/elsewhere/agent".into()),
        ("PI_CONFIG_DIR", ".other".into()),
        ("XDG_DATA_HOME", data.into()),
    ]);
    assert_eq!(
        redirected.overrides(&home),
        [
            "OMP_PROFILE",
            "PI_PROFILE",
            "PI_CODING_AGENT_DIR",
            "PI_CONFIG_DIR",
            "XDG_DATA_HOME"
        ]
    );
    let child = redirected.child(&home, Path::new("/opt/omp/bin/omp"));
    assert_eq!(
        child.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
        ["HOME", "PATH", "TERM", "NO_COLOR", "TMPDIR"]
    );
    assert_eq!(child[0].1, home.clone().into_os_string());
    assert_eq!(child[1].1, OsString::from("/opt/omp/bin:/usr/bin"));
}

#[test]
fn preparation_publishes_one_verified_private_copy_and_never_replaces_it() {
    let fixture = Fixture::new();
    let included = payload::included().unwrap();
    let prepared = payload::prepare(&fixture.data, included).unwrap();
    assert_eq!(prepared, fixture.prepared());
    let mut expected: Vec<_> = FILES
        .iter()
        .map(|(name, bytes)| ((*name).to_owned(), bytes.to_vec()))
        .collect();
    expected.sort();
    assert_eq!(snapshot(&prepared), expected);
    for (name, _) in FILES {
        let mode = fs::metadata(prepared.join(name))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(payload::prepare(&fixture.data, included).unwrap(), prepared);
    let names: Vec<_> = fs::read_dir(payload::base(&fixture.data))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(names, vec![prepared.file_name().unwrap().to_owned()]);
    fs::OpenOptions::new()
        .append(true)
        .open(prepared.join("index.mjs"))
        .unwrap()
        .write_all(b"// changed")
        .unwrap();
    let changed = fs::read(prepared.join("index.mjs")).unwrap();
    assert_eq!(
        payload::prepare(&fixture.data, included)
            .unwrap_err()
            .category,
        "PluginPayload"
    );
    assert_eq!(fs::read(prepared.join("index.mjs")).unwrap(), changed);
}

#[test]
fn a_registered_staging_copy_is_not_a_managed_release() {
    let fixture = Fixture::new();
    let base = payload::base(&fixture.data);
    crate::storage::private_directory(&base).unwrap();
    let staging = base.join(".omp-payload-1-1");
    create_private_directory(&staging).unwrap();
    for (name, bytes) in FILES {
        write_private(&staging.join(name), bytes).unwrap();
    }
    fixture.register(&staging);
    let view = fixture.inspect();
    assert_eq!(view.state, Conflict);
    assert_eq!(view.issue.unwrap().category, "PluginPayloadIntegrity");
    assert!(!view.installed.unwrap().managed);
    assert_eq!(view.actions, [Uninstall]);
}

#[test]
fn missing_ambiguous_overridden_old_or_incapable_targets_refuse_mutation() {
    let refused = |fixture: &Fixture, executable: Option<&str>, state, category: &str| {
        let view = fixture.manager.inspect(executable).unwrap();
        assert_eq!(view.state, state, "{category}");
        assert_eq!(view.issue.as_ref().unwrap().category, category);
        assert!(view.actions.is_empty() && view.observation.is_none());
        view
    };
    let unsupported = Fixture::build(false, |_, _| {});
    refused(&unsupported, None, Unsupported, "PluginPlatform");
    let missing = Fixture::with(|base, environment| environment.search = vec![base.join("none")]);
    refused(&missing, None, Missing, "PluginExecutableMissing");
    refused(
        &missing,
        Some("relative/omp"),
        Missing,
        "PluginExecutableInvalid",
    );
    let ambiguous = Fixture::with(|base, environment| {
        let other = base.join("other");
        executable(&other.join("omp"));
        environment.search.push(other);
    });
    let view = refused(&ambiguous, None, Unsupported, "PluginTargetAmbiguous");
    assert_eq!(
        view.issue.unwrap().context["candidates"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let selected = ambiguous.bin.join("omp");
    let view = ambiguous
        .manager
        .inspect(Some(selected.to_str().unwrap()))
        .unwrap();
    assert_eq!(view.state, Absent);
    assert_eq!(view.target.unwrap().executable, display(&selected));
    let overridden = Fixture::with(|_, environment| {
        environment.variables.push(("OMP_PROFILE", "work".into()));
    });
    let view = refused(&overridden, None, Unsupported, "PluginTargetOverride");
    assert_eq!(
        view.issue.unwrap().context["variables"],
        json!(["OMP_PROFILE"])
    );
    let old = Fixture::new();
    *lock(&old.fake.version) = "omp/18.8.7-rc.1".into();
    let view = refused(&old, None, Incompatible, "PluginVersionTooOld");
    assert_eq!(view.target.unwrap().version, "18.8.7-rc.1");
    let incapable = Fixture::new();
    *lock(&incapable.fake.help) = HELP.replace("uninstall", "remove");
    refused(&incapable, None, Incompatible, "PluginCapabilityMissing");
    let foreign = Fixture::new();
    *lock(&foreign.fake.version) = "Python 3.14.7".into();
    refused(&foreign, None, Unsupported, "PluginVersionUnavailable");
    for fixture in [
        &unsupported,
        &missing,
        &ambiguous,
        &overridden,
        &old,
        &incapable,
        &foreign,
    ] {
        assert_eq!(fixture.fake.mutations(), 0);
    }
}

#[test]
fn install_registers_the_prepared_payload_only_after_readback() {
    let fixture = Fixture::new();
    let unrelated = fixture.unrelated();
    let view = fixture.inspect();
    assert_eq!(view.state, Absent);
    assert_eq!(view.actions, [Install]);
    assert!(view.installed.is_none() && view.outcome.is_none());
    let target = view.target.clone().unwrap();
    assert_eq!(target.version, "18.8.9");
    assert_eq!(target.executable, display(&fixture.bin.join("omp")));
    assert_eq!(target.user_root, display(&fixture.home.join(".omp")));
    let applied = fixture.apply(&view, Install).unwrap();
    let outcome = applied.outcome.unwrap();
    assert_eq!((outcome.action, outcome.status), (Install, Verified));
    assert!(outcome.issue.is_none());
    assert_eq!(applied.state, Current);
    assert_eq!(applied.actions, [Uninstall]);
    assert_eq!(
        applied.installed.unwrap(),
        PluginInstallation {
            version: payload::included().unwrap().version.to_string(),
            source: display(&fixture.prepared()),
            content: Some(payload::included().unwrap().content.clone()),
            managed: true,
        }
    );
    assert_eq!(
        fs::read_link(fixture.registration()).unwrap(),
        fixture.prepared()
    );
    assert_eq!(
        fs::read_link(fixture.registry().join(UNRELATED)).unwrap(),
        unrelated
    );
    assert!(!fixture.journal().exists());
    assert!(applied.observation.is_some() && applied.observation != view.observation);
    assert_eq!(
        fixture.apply(&view, Install).unwrap_err().category,
        "PluginStale"
    );
}

#[test]
fn update_replaces_only_the_registration_and_keeps_the_older_payload() {
    let fixture = Fixture::new();
    let unrelated = fixture.unrelated();
    let older = fixture.managed(PREDECESSOR, FILES[1].1);
    let before = snapshot(&older);
    fixture.register(&older);
    let view = fixture.inspect();
    assert_eq!(view.state, PluginState::UpdateAvailable);
    assert_eq!(view.actions, [Update, Uninstall]);
    let installed = view.installed.clone().unwrap();
    assert_eq!(installed.version, PREDECESSOR);
    assert!(installed.managed);
    let applied = fixture.apply(&view, Update).unwrap();
    assert_eq!(applied.outcome.unwrap().status, Verified);
    assert_eq!(applied.state, Current);
    assert_eq!(
        fs::read_link(fixture.registration()).unwrap(),
        fixture.prepared()
    );
    assert_eq!(snapshot(&older), before);
    assert_eq!(
        fs::read_link(fixture.registry().join(UNRELATED)).unwrap(),
        unrelated
    );
}

#[test]
fn identical_newer_or_same_version_different_content_is_never_replaced() {
    let fixture = Fixture::new();
    let included = payload::included().unwrap();
    fixture.register(&payload::prepare(&fixture.data, included).unwrap());
    let current = fixture.inspect();
    assert_eq!(current.state, Current);
    assert_eq!(current.actions, [Uninstall]);
    assert!(current.issue.is_none());
    assert_eq!(
        fixture.apply(&current, Update).unwrap_err().category,
        "PluginActionUnavailable"
    );
    let cases = [
        (fixture.managed("999.0.0", FILES[1].1), "PluginNewerInstalled"),
        (
            fixture.managed(&included.version.to_string(), b"// other bytes"),
            "PluginContentMismatch",
        ),
        (
            fixture.checkout("newer", "999.0.0", FILES[1].1),
            "PluginNewerInstalled",
        ),
        (
            fixture.checkout("edited", &included.version.to_string(), b"// edited"),
            "PluginContentMismatch",
        ),
    ];
    for (source, category) in cases {
        fixture.register(&source);
        let view = fixture.inspect();
        assert_eq!(view.state, Conflict, "{category}");
        assert_eq!(view.issue.unwrap().category, category);
        assert_eq!(view.actions, [Uninstall]);
    }
    remove(&fixture.registration());
    fs::create_dir_all(fixture.registration()).unwrap();
    fs::write(
        fixture.registration().join("package.json"),
        manifest(PREDECESSOR),
    )
    .unwrap();
    let copied = fixture.inspect();
    assert_eq!(copied.state, Conflict);
    assert_eq!(copied.issue.unwrap().category, "PluginRegistrationNotLink");
    assert!(copied.actions.is_empty());
    assert_eq!(fixture.fake.mutations(), 0);
}

#[test]
fn only_a_recognized_predecessor_checkout_is_migrated_and_it_is_preserved() {
    let fixture = Fixture::new();
    let checkout = fixture.predecessor("checkout");
    let before = snapshot(&checkout);
    fixture.register(&checkout);
    let view = fixture.inspect();
    assert_eq!(view.state, Legacy);
    assert_eq!(view.actions, [Migrate, Uninstall]);
    let installed = view.installed.clone().unwrap();
    assert_eq!(installed.source, display(&checkout));
    assert_eq!(installed.version, PREDECESSOR);
    assert!(!installed.managed);
    let applied = fixture.apply(&view, Migrate).unwrap();
    assert_eq!(applied.outcome.unwrap().status, Verified);
    assert_eq!(applied.state, Current);
    assert_eq!(snapshot(&checkout), before);
    let edited = fixture.checkout("edited", PREDECESSOR, b"export default () => {};");
    fixture.register(&edited);
    let view = fixture.inspect();
    assert_eq!(view.state, Conflict);
    assert_eq!(
        view.issue.as_ref().unwrap().category,
        "PluginUnrecognizedSource"
    );
    assert_eq!(view.actions, [Uninstall]);
    assert_eq!(
        fixture.apply(&view, Migrate).unwrap_err().category,
        "PluginActionUnavailable"
    );
}

#[test]
fn a_changed_target_or_superseded_review_refuses_without_mutation() {
    let fixture = Fixture::new();
    let checkout = fixture.predecessor("checkout");
    fixture.register(&checkout);
    let first = fixture.inspect();
    let second = fixture.inspect();
    assert_eq!(
        fixture.apply(&first, Migrate).unwrap_err().category,
        "PluginStale"
    );
    // Any refused call consumes the current review too.
    assert_eq!(
        fixture.apply(&second, Migrate).unwrap_err().category,
        "PluginStale"
    );

    let review = fixture.inspect();
    fs::OpenOptions::new()
        .append(true)
        .open(checkout.join("index.mjs"))
        .unwrap()
        .write_all(b"\n// edited after review")
        .unwrap();
    let refused = fixture.apply(&review, Migrate).unwrap();
    let outcome = refused.outcome.unwrap();
    assert_eq!(outcome.status, Failed);
    assert_eq!(outcome.issue.unwrap().category, "PluginStale");
    assert_eq!(refused.state, Conflict);

    let replacement = fixture.predecessor("replacement");
    fixture.register(&replacement);
    let review = fixture.inspect();
    assert_eq!(review.state, Legacy);
    fs::write(fixture.bin.join("omp"), b"replaced executable").unwrap();
    let refused = fixture.apply(&review, Migrate).unwrap();
    let outcome = refused.outcome.unwrap();
    assert_eq!(outcome.status, Failed);
    assert_eq!(outcome.issue.unwrap().category, "PluginStale");
    assert_eq!(fixture.fake.mutations(), 0);
    assert_eq!(fs::read_link(fixture.registration()).unwrap(), replacement);
    assert!(!fixture.journal().exists());
}

#[test]
fn uninstall_removes_only_the_registration_and_keeps_every_source() {
    let fixture = Fixture::new();
    let unrelated = fixture.unrelated();
    let installed = fixture.apply(&fixture.inspect(), Install).unwrap();
    let prepared = snapshot(&fixture.prepared());
    let removed = fixture.apply(&installed, Uninstall).unwrap();
    let outcome = removed.outcome.unwrap();
    assert_eq!((outcome.action, outcome.status), (Uninstall, Verified));
    assert_eq!(removed.state, Absent);
    assert_eq!(removed.actions, [Install]);
    assert!(fs::symlink_metadata(fixture.registration()).is_err());
    assert_eq!(snapshot(&fixture.prepared()), prepared);
    assert_eq!(
        fs::read_link(fixture.registry().join(UNRELATED)).unwrap(),
        unrelated
    );

    let checkout = fixture.checkout("checkout", PREDECESSOR, FILES[1].1);
    let before = snapshot(&checkout);
    fixture.register(&checkout);
    let removed = fixture.apply(&fixture.inspect(), Uninstall).unwrap();
    assert_eq!(removed.outcome.unwrap().status, Verified);
    assert_eq!(snapshot(&checkout), before);
}

#[test]
fn failed_contradictory_or_interrupted_results_are_never_success() {
    let fixture = Fixture::new();
    let cases = [
        (Behavior::Ignore, Unknown, "PluginReadback", Absent),
        (Behavior::Refuse, Failed, "PluginCommandFailed", Absent),
        (Behavior::FailAfter, Unknown, "PluginCommandFailed", Current),
        (Behavior::Timeout, Unknown, "PluginTimeout", Current),
    ];
    for (behavior, status, category, state) in cases {
        remove(&fixture.registration());
        let view = fixture.inspect();
        assert_eq!(view.state, Absent);
        fixture.fake.script(behavior);
        let applied = fixture.apply(&view, Install).unwrap();
        let outcome = applied.outcome.unwrap();
        assert_eq!(outcome.status, status, "{category}");
        assert_eq!(outcome.issue.unwrap().category, category);
        assert_eq!(applied.state, state, "{category}");
        assert!(!fixture.journal().exists());
    }
    // Each admitted action dispatched exactly once; nothing was replayed.
    assert_eq!(fixture.fake.mutations(), 4);
}

#[test]
fn a_marketplace_plugin_with_the_adapter_name_refuses_management() {
    for id in [NAME.to_owned(), format!("{NAME}@market")] {
        let fixture = Fixture::new();
        lock(&fixture.fake.marketplace).push(json!({"id": id, "enabled": true}));
        let view = fixture.inspect();
        assert_eq!(view.state, Conflict);
        assert_eq!(view.issue.unwrap().category, "PluginInventoryCollision");
        assert!(view.actions.is_empty() && view.observation.is_none());
    }
}

#[test]
fn a_pending_mutation_refuses_conflicting_work_until_it_settles() {
    let fixture = Fixture::new();
    let view = fixture.inspect();
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    fixture.fake.script(Behavior::Hold(entered, released));
    let manager = fixture.manager.clone();
    let token = view.observation.clone().unwrap();
    let pending = std::thread::spawn(move || manager.apply(&token, Install));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(
        fixture.manager.inspect(None).unwrap_err().category,
        "PluginBusy"
    );
    assert_eq!(
        fixture.apply(&view, Install).unwrap_err().category,
        "PluginBusy"
    );
    release.send(()).unwrap();
    let applied = pending.join().unwrap().unwrap();
    assert_eq!(applied.outcome.unwrap().status, Verified);
    assert_eq!(fixture.fake.mutations(), 1);
    // A later inspection still reports the settled result.
    let reopened = fixture.inspect();
    assert_eq!(reopened.outcome.unwrap().status, Verified);
    assert_eq!(reopened.state, Current);
}

#[test]
fn shutdown_interrupts_its_own_child_and_the_next_run_reports_unknown() {
    let fixture = Fixture::new();
    let view = fixture.inspect();
    let (entered, reached) = mpsc::channel();
    fixture.fake.script(Behavior::AwaitCancel(entered));
    let manager = fixture.manager.clone();
    let token = view.observation.clone().unwrap();
    let pending = std::thread::spawn(move || manager.apply(&token, Install));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    let started = Instant::now();
    fixture.manager.shutdown(Duration::ZERO);
    assert!(started.elapsed() < Duration::from_secs(5));
    let interrupted = pending.join().unwrap().unwrap();
    let outcome = interrupted.outcome.unwrap();
    assert_eq!(outcome.status, Unknown);
    assert_eq!(outcome.issue.unwrap().category, "PluginInterrupted");
    assert!(interrupted.observation.is_none());
    assert!(fixture.journal().exists());
    assert_eq!(
        fixture.manager.inspect(None).unwrap_err().category,
        "Closing"
    );

    let next = PluginManager::with_client(
        Ok(fixture.data.clone()),
        fixture.environment(),
        true,
        Box::new(Shared(Fake::new())),
    );
    let view = next.inspect(None).unwrap();
    let outcome = view.outcome.unwrap();
    assert_eq!((outcome.action, outcome.status), (Install, Unknown));
    assert_eq!(outcome.issue.unwrap().category, "PluginInterrupted");
    // The outcome keeps the journaled target; the view is the current readback.
    assert_eq!(
        outcome.target.unwrap().user_root,
        display(&fixture.home.join(".omp"))
    );
    assert!(!fixture.journal().exists());
    assert_eq!(view.state, Absent);
}

#[test]
fn shutdown_waits_for_an_independent_inspection_to_settle() {
    struct PendingInspection {
        entered: mpsc::Sender<()>,
        cancelled: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }
    impl Client for PendingInspection {
        fn run(
            &self,
            invocation: &Invocation<'_>,
            _operation: Operation<'_>,
        ) -> Result<Output, RunError> {
            self.entered.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            while !invocation.cancel.load(Ordering::Acquire) {
                assert!(Instant::now() < deadline, "inspection was not cancelled");
                std::thread::sleep(Duration::from_millis(5));
            }
            self.cancelled.send(()).unwrap();
            lock(&self.release)
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
            Err(RunError::Started(process::interrupted()))
        }
    }

    let fixture = Fixture::new();
    let (entered, reached) = mpsc::channel();
    let (cancelled, cancellation) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let manager = Arc::new(PluginManager::with_client(
        Ok(fixture.data.clone()),
        fixture.environment(),
        true,
        Box::new(PendingInspection {
            entered,
            cancelled,
            release: Mutex::new(released),
        }),
    ));
    let inspecting = manager.clone();
    let inspection = std::thread::spawn(move || inspecting.inspect(None));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    let (settled, settlement) = mpsc::channel();
    let closing = manager.clone();
    let shutdown = std::thread::spawn(move || {
        closing.shutdown(Duration::ZERO);
        settled.send(()).unwrap();
    });
    cancellation.recv_timeout(Duration::from_secs(10)).unwrap();
    let returned_early = settlement.recv_timeout(Duration::from_millis(100)).is_ok();
    release.send(()).unwrap();
    shutdown.join().unwrap();
    assert_eq!(inspection.join().unwrap().unwrap_err().category, "Closing");
    assert!(
        !returned_early,
        "shutdown returned while inspection still owned work"
    );
    drop(target::lease(&fixture.home).unwrap());
}

const PROCESS_FIXTURE: &str = "MADO_PLUGIN_PROCESS_FIXTURE";
const PROCESS_MARKERS: &str = "MADO_PLUGIN_PROCESS_MARKERS";
const FIXTURE_ARGUMENTS: [&str; 3] = [
    "--exact",
    "plugin_management::tests::process_fixture",
    "--nocapture",
];

/// Child-side behavior for the runner tests; does nothing in a normal run.
///
/// A leader first starts a silent companion, as OMP starts Bun, and waits until
/// it runs. It then exits, sleeps, floods stdout, or exits while the companion
/// holds its inherited output. A companion or unrelated process writes its late
/// marker only after the test releases it, so the write proves it outlived a run.
#[test]
fn process_fixture() {
    let (Ok(mode), Some(markers)) = (
        std::env::var(PROCESS_FIXTURE),
        std::env::var_os(PROCESS_MARKERS),
    ) else {
        return;
    };
    let markers = PathBuf::from(markers);
    if matches!(mode.as_str(), "companion" | "unrelated") {
        fs::write(markers.join(format!("{mode}.started")), b"").unwrap();
        if appears(&markers.join("released"), Duration::from_secs(30)) {
            fs::write(markers.join(format!("{mode}.late")), b"").unwrap();
        }
        return;
    }
    let mut companion = fixture_command("companion", &markers);
    if mode != "hold" {
        companion
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    companion.spawn().unwrap();
    assert!(appears(
        &markers.join("companion.started"),
        Duration::from_secs(10)
    ));
    match mode.as_str() {
        "sleep" => std::thread::sleep(Duration::from_secs(30)),
        "flood" => {
            let chunk = [b'x'; 65_536];
            let mut stdout = std::io::stdout().lock();
            while stdout.write_all(&chunk).is_ok() {}
        }
        "exit" | "hold" => {}
        other => panic!("unknown process fixture {other}"),
    }
}

fn fixture_command(mode: &str, markers: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(FIXTURE_ARGUMENTS)
        .env(PROCESS_FIXTURE, mode)
        .env(PROCESS_MARKERS, markers)
        .stdin(std::process::Stdio::null());
    command
}

/// Waits a bounded time for a fixture marker.
fn appears(path: &Path, bound: Duration) -> bool {
    let deadline = Instant::now() + bound;
    while !path.exists() {
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}

fn fixture_run(
    mode: &str,
    markers: &Path,
    limits: Limits,
    cancel: &AtomicBool,
) -> Result<Output, RunError> {
    process::run(
        &std::env::current_exe().unwrap(),
        &FIXTURE_ARGUMENTS.map(OsStr::new),
        markers,
        &[
            (PROCESS_FIXTURE, mode.into()),
            (PROCESS_MARKERS, markers.into()),
        ],
        limits,
        cancel,
    )
}

fn started(result: Result<Output, RunError>) -> String {
    match result {
        Err(RunError::Started(fault)) => fault.category,
        Err(RunError::NotStarted(fault)) => panic!("unexpected start failure {fault:?}"),
        Err(RunError::Unsettled(fault)) => panic!("the owned process group remained {fault:?}"),
        Ok(_) => panic!("the bounded run must settle its child early"),
    }
}

/// A process the runner does not own, left in the test's own process group like
/// an OMP session the operator started.
struct Unrelated {
    child: std::process::Child,
    markers: Root,
}

impl Unrelated {
    fn start() -> Self {
        let markers = Root::new();
        let child = fixture_command("unrelated", &markers.0)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        assert!(appears(
            &markers.0.join("unrelated.started"),
            Duration::from_secs(10)
        ));
        Self { child, markers }
    }

    /// Releases every fixture process. The unrelated one still writes its late
    /// marker; no owned companion can, because each run ended it before returning.
    fn release(mut self, owned: &[&PathBuf]) {
        for markers in owned.iter().copied().chain([&self.markers.0]) {
            fs::write(markers.join("released"), b"").unwrap();
        }
        assert!(appears(
            &self.markers.0.join("unrelated.late"),
            Duration::from_secs(10)
        ));
        assert!(self.child.wait().unwrap().success());
        // Companions poll like the unrelated process; a survivor has written by now.
        std::thread::sleep(Duration::from_millis(500));
        for markers in owned {
            assert!(markers.join("companion.started").exists());
            assert!(
                !markers.join("companion.late").exists(),
                "an owned companion outlived its run: {}",
                markers.display()
            );
        }
    }
}

#[test]
fn deadline_and_cancellation_end_only_the_owned_process_group() {
    let unrelated = Unrelated::start();
    let limits = Limits {
        duration: Duration::from_secs(60),
        stdout: 1024 * 1024,
        stderr: 64 * 1024,
    };
    let expired = Root::new();
    let short = Limits {
        duration: Duration::from_secs(3),
        ..limits
    };
    let begun = Instant::now();
    assert_eq!(
        started(fixture_run(
            "sleep",
            &expired.0,
            short,
            &AtomicBool::new(false)
        )),
        "PluginTimeout"
    );
    assert!(begun.elapsed() < Duration::from_secs(10));
    let cancelled = Root::new();
    let cancel = AtomicBool::new(false);
    let settling = std::thread::scope(|scope| {
        let canceller = scope.spawn(|| {
            appears(
                &cancelled.0.join("companion.started"),
                Duration::from_secs(10),
            );
            cancel.store(true, Ordering::Release);
            Instant::now()
        });
        assert_eq!(
            started(fixture_run("sleep", &cancelled.0, limits, &cancel)),
            "PluginInterrupted"
        );
        canceller.join().unwrap().elapsed()
    });
    assert!(settling < Duration::from_secs(5));
    unrelated.release(&[&expired.0, &cancelled.0]);
}

#[test]
fn exit_output_and_grace_results_leave_no_owned_process_running() {
    let unrelated = Unrelated::start();
    let limits = Limits {
        duration: Duration::from_secs(60),
        stdout: 1024 * 1024,
        stderr: 64 * 1024,
    };
    let normal = Root::new();
    match fixture_run("exit", &normal.0, limits, &AtomicBool::new(false)) {
        Ok(done) => assert!(done.success),
        Err(error) => panic!("the fixture leader must complete {:?}", error.fault()),
    }
    let flooded = Root::new();
    let small = Limits {
        stdout: 1024,
        ..limits
    };
    assert_eq!(
        started(fixture_run(
            "flood",
            &flooded.0,
            small,
            &AtomicBool::new(false)
        )),
        "PluginOutputLimit"
    );
    let held = Root::new();
    assert_eq!(
        started(fixture_run(
            "hold",
            &held.0,
            limits,
            &AtomicBool::new(false)
        )),
        "PluginProcess"
    );
    unrelated.release(&[&normal.0, &flooded.0, &held.0]);
    match process::run(
        Path::new("/nonexistent/omp"),
        &[],
        &std::env::temp_dir(),
        &[],
        limits,
        &AtomicBool::new(false),
    ) {
        Err(RunError::NotStarted(fault)) => assert_eq!(fault.category, "PluginSpawn"),
        _ => panic!("a missing executable must not start"),
    }
}

#[test]
fn an_outcome_retains_its_target_after_another_executable_is_inspected() {
    let fixture = Fixture::new();
    let reviewed = fixture.inspect();
    let target = reviewed.target.clone();
    let installed = fixture.apply(&reviewed, Install).unwrap();
    assert_eq!(installed.outcome.unwrap().target, target);
    let other = fixture.bin.join("other-omp");
    executable(&other);
    let next = fixture
        .manager
        .inspect(Some(other.to_str().unwrap()))
        .unwrap();
    assert_eq!(next.target.unwrap().executable, display(&other));
    assert_eq!(next.outcome.unwrap().target, target);
}

#[test]
fn supported_dotenv_directory_selection_refuses_before_any_client_run() {
    let cases = [
        // The reproduced case: the agent file selects an existing XDG data root.
        (
            ".omp/agent/.env",
            "OPENAI_API_KEY=sk-owned\nXDG_DATA_HOME={xdg}\n",
        ),
        (".omp/.env", "export XDG_DATA_HOME=\"{xdg}\" # data\n"),
        (".env", "OMP_PROFILE=work\n"),
        (".env.local", "PI_CODING_AGENT_DIR=/elsewhere/agent\n"),
        (".env.development", "OMP_CONFIG_DIR = .other\n"),
        (".env.development.local", "XDG_DATA_HOME: {xdg}\r\n"),
        // An expanded value cannot be shown to keep the normal target.
        (".env", "XDG_DATA_HOME=${HOME}/share\n"),
    ];
    for (file, contents) in cases {
        let fixture = Fixture::new();
        let xdg = fixture.base.join("xdg");
        let custom = xdg.join("omp/plugins/node_modules");
        fs::create_dir_all(&custom).unwrap();
        let path = fixture.home.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents.replace("{xdg}", &display(&xdg))).unwrap();
        let view = fixture.inspect();
        assert_eq!(view.state, Unsupported, "{file}: {contents}");
        let issue = view.issue.unwrap();
        assert_eq!(issue.category, "PluginTargetOverride", "{file}: {contents}");
        assert_eq!(issue.context["dotenv"][0]["file"], display(&path));
        assert!(view.actions.is_empty() && view.observation.is_none());
        assert_eq!(fixture.fake.runs(), 0, "{file}: {contents}");
        assert!(fs::read_dir(&custom).unwrap().next().is_none());
    }
    // A linked dotfile is read as the client reads it.
    let linked = Fixture::new();
    let xdg = linked.base.join("xdg");
    fs::create_dir_all(xdg.join("omp")).unwrap();
    let dotfile = linked.base.join("dotfiles-env");
    fs::write(&dotfile, format!("XDG_DATA_HOME={}\n", display(&xdg))).unwrap();
    symlink(&dotfile, linked.home.join(".env")).unwrap();
    assert_eq!(
        linked.inspect().issue.unwrap().category,
        "PluginTargetOverride"
    );
    // A file beyond the bound is not partially trusted.
    let oversized = Fixture::new();
    fs::write(oversized.home.join(".env"), vec![b'#'; 256 * 1024 + 1]).unwrap();
    let view = oversized.inspect();
    assert_eq!(view.state, Unsupported);
    assert_eq!(view.issue.unwrap().category, "PluginTargetUnverified");
    for fixture in [&linked, &oversized] {
        assert_eq!(fixture.fake.runs(), 0);
    }
}

#[test]
fn ordinary_dotenv_keeps_the_normal_target_and_a_later_redirect_is_refused() {
    let fixture = Fixture::new();
    let xdg = fixture.base.join("xdg");
    let custom = xdg.join("omp/plugins/node_modules");
    fs::create_dir_all(&custom).unwrap();
    let agent = fixture.home.join(".omp/agent");
    fs::create_dir_all(&agent).unwrap();
    fs::write(
        fixture.home.join(".env"),
        format!(
            "# XDG_DATA_HOME={xdg}\nOPENAI_API_KEY=\"sk-owned#1\"\nNOTE='XDG_DATA_HOME={xdg}'\n\
             export OMP_PROFILE=default\nPI_CODING_AGENT_DIR={agent}\nPI_CONFIG_DIR=.omp\n\
             XDG_DATA_HOME={empty}\n",
            xdg = display(&xdg),
            agent = display(&agent),
            empty = display(&fixture.base.join("empty")),
        ),
    )
    .unwrap();
    // A directory is not a dotenv file.
    fs::create_dir(fixture.home.join(".env.local")).unwrap();
    let view = fixture.inspect();
    assert_eq!(view.state, Absent);
    let installed = fixture.apply(&view, Install).unwrap();
    assert_eq!(installed.outcome.as_ref().unwrap().status, Verified);
    assert_eq!(
        fs::read_link(fixture.registration()).unwrap(),
        fixture.prepared()
    );

    // Routing changed after review: the reviewed action is refused undispatched.
    fs::write(
        agent.join(".env"),
        format!("XDG_DATA_HOME={}\n", display(&xdg)),
    )
    .unwrap();
    let refused = fixture.apply(&installed, Uninstall).unwrap();
    let outcome = refused.outcome.unwrap();
    assert_eq!(outcome.status, Failed);
    assert_eq!(outcome.issue.unwrap().category, "PluginStale");
    assert_eq!(refused.state, Unsupported);
    assert_eq!(refused.issue.unwrap().category, "PluginTargetOverride");
    assert!(refused.observation.is_none());
    assert_eq!(fixture.fake.mutations(), 1);
    assert_eq!(
        fs::read_link(fixture.registration()).unwrap(),
        fixture.prepared()
    );
    assert!(fs::read_dir(&custom).unwrap().next().is_none());
    assert!(!fixture.journal().exists());
}

#[test]
fn installation_ownership_spans_data_roots_until_the_owner_settles() {
    let fixture = Fixture::new();
    let (peer_data, peer_fake, peer) = fixture.peer("peer-data");
    let elsewhere = Fixture::new();
    let installed = fixture.apply(&fixture.inspect(), Install).unwrap();
    assert_eq!(installed.state, Current);
    // The peer recognizes the first instance's release as a source to migrate.
    let reviewed = peer.inspect(None).unwrap();
    assert_eq!(reviewed.state, Legacy);
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    fixture.fake.script(Behavior::Hold(entered, released));
    let manager = fixture.manager.clone();
    let token = installed.observation.clone().unwrap();
    let pending = std::thread::spawn(move || manager.apply(&token, Uninstall));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();

    assert_eq!(
        target::lease(&fixture.home).unwrap_err().category,
        "PluginInstallationBusy"
    );
    let runs = peer_fake.runs();
    assert_eq!(
        peer.apply(reviewed.observation.as_deref().unwrap(), Migrate)
            .unwrap_err()
            .category,
        "PluginInstallationBusy"
    );
    // Refused at admission: no command, payload, journal or outcome.
    assert_eq!((peer_fake.runs(), peer_fake.mutations()), (runs, 0));
    assert!(!payload::base(&peer_data).exists());
    assert!(!journal_path(&peer_data).exists());
    assert_eq!(
        fs::read_link(fixture.registration()).unwrap(),
        fixture.prepared()
    );
    // Another HOME is another installation.
    let other = elsewhere.apply(&elsewhere.inspect(), Install).unwrap();
    assert_eq!(other.outcome.unwrap().status, Verified);

    release.send(()).unwrap();
    let removed = pending.join().unwrap().unwrap();
    assert_eq!(removed.outcome.unwrap().status, Verified);
    drop(target::lease(&fixture.home).unwrap());
    // The refused call consumed its review; only a fresh review resumes.
    assert_eq!(
        peer.apply(reviewed.observation.as_deref().unwrap(), Migrate)
            .unwrap_err()
            .category,
        "PluginStale"
    );
    let resumed = peer.inspect(None).unwrap();
    assert_eq!(resumed.state, Absent);
    assert!(resumed.outcome.is_none());
    let applied = peer
        .apply(resumed.observation.as_deref().unwrap(), Install)
        .unwrap();
    assert_eq!(applied.outcome.unwrap().status, Verified);
    let included = payload::included().unwrap();
    assert_eq!(
        fs::read_link(fixture.registration()).unwrap(),
        payload::base(&peer_data).join(format!("{}-{}", included.version, included.content))
    );
    assert_eq!(peer_fake.mutations(), 1);
}

#[test]
fn an_unsettled_command_tree_refuses_the_installation_until_it_is_reconciled() {
    let fixture = Fixture::new();
    let (_, peer_fake, peer) = fixture.peer("peer-data");
    let elsewhere = Fixture::new();
    let marker = fixture.home.join(".omp/mado-mata-plugin-unsettled.json");
    let view = fixture.inspect();
    // Reviewed before the tree was left unsettled.
    let reviewed = peer.inspect(None).unwrap();
    assert_eq!(reviewed.state, Absent);
    fixture.fake.script(Behavior::Unsettled);
    let applied = fixture.apply(&view, Install).unwrap();
    let outcome = applied.outcome.unwrap();
    assert_eq!(outcome.status, Unknown);
    assert_eq!(outcome.issue.unwrap().category, "PluginUnsettled");
    assert_eq!(applied.issue.unwrap().category, "PluginUnsettled");
    assert!(applied.observation.is_none() && applied.actions.is_empty());
    assert!(marker.is_file());
    // No settled result exists, so the next run of this data root reports it too.
    assert!(fixture.journal().exists());
    // The record, not a held lease, refuses the installation.
    drop(target::lease(&fixture.home).unwrap());

    // Every instance and data root refuses before any client run, after restart too.
    let runs = (fixture.fake.runs(), peer_fake.runs());
    let refused = peer
        .apply(reviewed.observation.as_deref().unwrap(), Install)
        .unwrap();
    let outcome = refused.outcome.unwrap();
    assert_eq!(outcome.status, Failed);
    assert_eq!(outcome.issue.unwrap().category, "PluginStale");
    assert_eq!(refused.issue.unwrap().category, "PluginUnsettled");
    let restarted_fake = Fake::new();
    let restarted = PluginManager::with_client(
        Ok(fixture.data.clone()),
        fixture.environment(),
        true,
        Box::new(Shared(restarted_fake.clone())),
    );
    let reopened = restarted.inspect(None).unwrap();
    assert_eq!(reopened.outcome.as_ref().unwrap().status, Unknown);
    for view in [fixture.inspect(), peer.inspect(None).unwrap(), reopened] {
        assert_eq!(view.state, Unsupported);
        assert_eq!(view.issue.unwrap().category, "PluginUnsettled");
        assert!(view.actions.is_empty() && view.observation.is_none());
    }
    assert_eq!((fixture.fake.runs(), peer_fake.runs()), runs);
    assert_eq!((restarted_fake.runs(), peer_fake.mutations()), (0, 0));
    assert_eq!(
        fs::read_link(fixture.registration()).unwrap(),
        fixture.prepared()
    );
    assert!(marker.is_file());
    let other = elsewhere.apply(&elsewhere.inspect(), Install).unwrap();
    assert_eq!(other.outcome.unwrap().status, Verified);

    // Only the operator's reconciliation resumes management, from a fresh review.
    fs::remove_file(&marker).unwrap();
    let resumed = peer.inspect(None).unwrap();
    assert_eq!(resumed.state, Legacy);
    let removed = peer
        .apply(resumed.observation.as_deref().unwrap(), Uninstall)
        .unwrap();
    assert_eq!(removed.outcome.unwrap().status, Verified);
    assert!(fs::symlink_metadata(fixture.registration()).is_err());
    assert!(fixture.prepared().is_dir());
}

#[test]
fn a_mutation_whose_fence_cannot_be_recorded_is_not_dispatched() {
    let fixture = Fixture::new();
    let view = fixture.inspect();
    let read_only = ReadOnly::new(&fixture.home.join(".omp"));
    let refused = fixture.apply(&view, Install).unwrap();
    let outcome = refused.outcome.unwrap();
    assert_eq!(outcome.status, Failed);
    assert_eq!(outcome.issue.unwrap().category, "PluginInstallationLock");
    assert_eq!(fixture.fake.mutations(), 0);
    assert!(!fixture.journal().exists());
    assert!(fs::symlink_metadata(fixture.registration()).is_err());
    drop(read_only);
    // Nothing is left behind: a fresh review installs normally.
    let installed = fixture.apply(&fixture.inspect(), Install).unwrap();
    assert_eq!(installed.outcome.unwrap().status, Verified);
    assert_eq!(fixture.fake.mutations(), 1);
}

#[test]
fn homes_whose_user_root_is_one_directory_share_its_ownership() {
    let fixture = Fixture::new();
    // Another HOME whose user root links to the same registry.
    let alias = Fixture::new();
    let user_root = alias.home.join(".omp");
    fs::remove_dir_all(&user_root).unwrap();
    symlink(fixture.home.join(".omp"), &user_root).unwrap();
    let elsewhere = Fixture::new();
    let reviewed = alias.inspect();
    assert_eq!(reviewed.state, Absent);
    let view = fixture.inspect();
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    fixture.fake.script(Behavior::Hold(entered, released));
    let manager = fixture.manager.clone();
    let token = view.observation.clone().unwrap();
    let pending = std::thread::spawn(move || manager.apply(&token, Install));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();

    assert_eq!(
        alias.apply(&reviewed, Install).unwrap_err().category,
        "PluginInstallationBusy"
    );
    // No read runs while another HOME of this installation owns it.
    let runs = alias.fake.runs();
    let pending_view = alias.inspect();
    assert_eq!(pending_view.state, Unsupported);
    assert_eq!(
        pending_view.issue.unwrap().category,
        "PluginInstallationBusy"
    );
    assert_eq!((alias.fake.runs(), alias.fake.mutations()), (runs, 0));
    // A distinct registry stays independent.
    let other = elsewhere.apply(&elsewhere.inspect(), Install).unwrap();
    assert_eq!(other.outcome.unwrap().status, Verified);

    release.send(()).unwrap();
    let applied = pending.join().unwrap().unwrap();
    assert_eq!(applied.outcome.unwrap().status, Verified);
    assert_eq!(
        fs::read_link(alias.registration()).unwrap(),
        fixture.prepared()
    );
    assert!(
        !fixture
            .home
            .join(".omp/mado-mata-plugin-unsettled.json")
            .exists()
    );
}

#[test]
fn a_concurrent_read_neither_runs_nor_loses_an_unsettled_refusal() {
    let fixture = Fixture::new();
    let (_, peer_fake, peer) = fixture.peer("peer-data");
    let marker = fixture.home.join(".omp/mado-mata-plugin-unsettled.json");
    let view = fixture.inspect();
    let reviewed = peer.inspect(None).unwrap();
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    fixture.fake.script(Behavior::Hold(entered, released));
    let manager = fixture.manager.clone();
    let token = view.observation.clone().unwrap();
    let pending = std::thread::spawn(move || manager.apply(&token, Install));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();

    // The peer's inventory tree would be left unsettled, but it cannot start.
    peer_fake.unsettled_list.store(true, Ordering::SeqCst);
    let runs = peer_fake.runs();
    assert_eq!(
        peer.apply(reviewed.observation.as_deref().unwrap(), Install)
            .unwrap_err()
            .category,
        "PluginInstallationBusy"
    );
    let blocked = peer.inspect(None).unwrap();
    assert_eq!(blocked.state, Unsupported);
    assert_eq!(blocked.issue.unwrap().category, "PluginInstallationBusy");
    assert!(blocked.observation.is_none());
    assert_eq!((peer_fake.runs(), peer_fake.mutations()), (runs, 0));

    release.send(()).unwrap();
    let applied = pending.join().unwrap().unwrap();
    assert_eq!(applied.outcome.unwrap().status, Verified);
    assert!(!marker.exists());
    // Once the owner settled, the peer's read runs under ownership, and its
    // unsettled tree stays recorded for every instance.
    let left = peer.inspect(None).unwrap();
    assert_eq!(left.state, Unsupported);
    assert_eq!(left.issue.unwrap().category, "PluginUnsettled");
    peer_fake.unsettled_list.store(false, Ordering::SeqCst);
    for view in [fixture.inspect(), peer.inspect(None).unwrap()] {
        assert_eq!(view.state, Unsupported);
        assert_eq!(view.issue.unwrap().category, "PluginUnsettled");
        assert!(view.observation.is_none());
    }
    assert!(marker.is_file());
}

#[test]
fn an_unrecorded_unsettled_inventory_keeps_ownership_in_its_instance() {
    let fixture = Fixture::new();
    let read_only = ReadOnly::new(&fixture.home.join(".omp"));
    fixture.fake.unsettled_list.store(true, Ordering::SeqCst);
    assert_eq!(
        fixture.manager.inspect(None).unwrap_err().category,
        "PluginUnsettled"
    );
    fixture.fake.unsettled_list.store(false, Ordering::SeqCst);
    let runs = fixture.fake.runs();
    assert_eq!(
        fixture.manager.inspect(None).unwrap_err().category,
        "PluginUnsettled"
    );
    assert_eq!(fixture.fake.runs(), runs);
    assert_eq!(
        target::lease(&fixture.home).unwrap_err().category,
        "PluginInstallationBusy"
    );
    drop(read_only);
}

#[test]
fn an_apply_refused_by_a_reading_owner_changes_nothing() {
    let fixture = Fixture::new();
    let (peer_data, peer_fake, peer) = fixture.peer("peer-data");
    let reviewed = peer.inspect(None).unwrap();
    assert_eq!(reviewed.state, Absent);
    // Another instance's inspection owns the installation while it reads.
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    lock(&fixture.fake.reads).push_back(Behavior::Hold(entered, released));
    let manager = fixture.manager.clone();
    let reading = std::thread::spawn(move || manager.inspect(None));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();

    let runs = peer_fake.runs();
    assert_eq!(
        peer.apply(reviewed.observation.as_deref().unwrap(), Install)
            .unwrap_err()
            .category,
        "PluginInstallationBusy"
    );
    assert_eq!((peer_fake.runs(), peer_fake.mutations()), (runs, 0));
    assert!(!payload::base(&peer_data).exists());
    assert!(!journal_path(&peer_data).exists());
    // The reading instance refuses its own concurrent work as pending.
    assert_eq!(
        fixture.manager.inspect(None).unwrap_err().category,
        "PluginBusy"
    );

    release.send(()).unwrap();
    let read = reading.join().unwrap().unwrap();
    assert_eq!(read.state, Absent);
    assert!(read.observation.is_some());
    assert!(fs::symlink_metadata(fixture.registration()).is_err());
    // The refused call consumed its review; only a fresh review resumes.
    assert_eq!(
        peer.apply(reviewed.observation.as_deref().unwrap(), Install)
            .unwrap_err()
            .category,
        "PluginStale"
    );
    let fresh = peer.inspect(None).unwrap();
    assert!(fresh.outcome.is_none());
    let applied = peer
        .apply(fresh.observation.as_deref().unwrap(), Install)
        .unwrap();
    assert_eq!(applied.outcome.unwrap().status, Verified);
    assert_eq!(peer_fake.mutations(), 1);
}

#[test]
fn an_inspection_interrupted_by_shutdown_starts_no_later_command() {
    let fixture = Fixture::new();
    let (entered, reached) = mpsc::channel();
    lock(&fixture.fake.reads).push_back(Behavior::AwaitCancel(entered));
    let manager = fixture.manager.clone();
    let inspection = std::thread::spawn(move || manager.inspect(None));
    reached.recv_timeout(Duration::from_secs(10)).unwrap();
    fixture.manager.shutdown(Duration::ZERO);
    assert_eq!(inspection.join().unwrap().unwrap_err().category, "Closing");
    // Only the probe in flight ran; nothing started after cancellation.
    assert_eq!((fixture.fake.runs(), fixture.fake.mutations()), (1, 0));
    assert!(!fixture.journal().exists());
    drop(target::lease(&fixture.home).unwrap());
}
