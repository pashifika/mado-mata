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
enum Behavior {
    /// Exits successfully without changing the registry.
    Ignore,
    /// Exits unsuccessfully without changing the registry.
    Refuse,
    /// Changes the registry, then exits unsuccessfully.
    FailAfter,
    /// Changes the registry, then the bounded run expires.
    Timeout,
    Hold(mpsc::Sender<()>, mpsc::Receiver<()>),
    AwaitCancel(mpsc::Sender<()>),
}

struct Fake {
    version: Mutex<String>,
    help: Mutex<String>,
    marketplace: Mutex<Vec<Value>>,
    behaviors: Mutex<VecDeque<Behavior>>,
    mutations: AtomicUsize,
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
            mutations: AtomicUsize::new(0),
        })
    }

    fn mutations(&self) -> usize {
        self.mutations.load(Ordering::SeqCst)
    }

    fn script(&self, behavior: Behavior) {
        lock(&self.behaviors).push_back(behavior);
    }

    fn run(
        &self,
        invocation: &Invocation<'_>,
        operation: Operation<'_>,
    ) -> Result<Output, RunError> {
        let registry = invocation.home.join(".omp/plugins/node_modules");
        match operation {
            Operation::Version => Ok(exited(true, lock(&self.version).clone().into_bytes())),
            Operation::Help => Ok(exited(true, lock(&self.help).clone().into_bytes())),
            Operation::List => Ok(exited(true, list(&registry, &lock(&self.marketplace)))),
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
                    Some(Behavior::Hold(entered, release)) => {
                        entered.send(()).unwrap();
                        release.recv().unwrap();
                        Ok(mutate(&registry, operation))
                    }
                    Some(Behavior::AwaitCancel(entered)) => {
                        entered.send(()).unwrap();
                        let deadline = Instant::now() + Duration::from_secs(10);
                        while !invocation.cancel.load(Ordering::Acquire) {
                            assert!(
                                Instant::now() < deadline,
                                "shutdown must interrupt the child"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(RunError::Started(process::interrupted()))
                    }
                }
            }
        }
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
fn the_build_supplies_one_finite_identified_adapter_release() {
    let included = payload::included().unwrap();
    assert_eq!(
        FILES.map(|(name, _)| name),
        ["package.json", "index.mjs", "client.mjs"]
    );
    let manifest: Value = serde_json::from_slice(FILES[0].1).unwrap();
    assert_eq!(manifest["name"], NAME);
    assert_eq!(included.version.to_string(), "0.1.1");
    assert_eq!(included.minimum.to_string(), "18.8.7");
    assert_eq!(included.content.len(), 64);
    assert_eq!(included.content, payload::identity(FILES));
    let mut changed = FILES.map(|(name, bytes)| (name, bytes.to_vec()));
    changed[1].1.push(b'\n');
    assert_ne!(
        payload::identity(
            changed
                .iter()
                .map(|(name, bytes)| (*name, bytes.as_slice()))
        ),
        included.content
    );
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
            version: "0.1.1".into(),
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
        (fixture.managed("0.1.2", FILES[1].1), "PluginNewerInstalled"),
        (
            fixture.managed("0.1.1", b"// other bytes"),
            "PluginContentMismatch",
        ),
        (
            fixture.checkout("newer", "0.2.0", FILES[1].1),
            "PluginNewerInstalled",
        ),
        (
            fixture.checkout("edited", "0.1.1", b"// edited"),
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
    let checkout = fixture.checkout("checkout", PREDECESSOR, FILES[1].1);
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
    let checkout = fixture.checkout("checkout", PREDECESSOR, FILES[1].1);
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

    let replacement = fixture.checkout("replacement", PREDECESSOR, FILES[1].1);
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
    let issue = outcome.issue.unwrap();
    assert_eq!(issue.category, "PluginInterrupted");
    assert_eq!(
        issue.context["user_root"],
        display(&fixture.home.join(".omp"))
    );
    assert!(!fixture.journal().exists());
    assert_eq!(view.state, Absent);
}

const PROCESS_FIXTURE: &str = "MADO_PLUGIN_PROCESS_FIXTURE";
const PROCESS_MARKER: &str = "MADO_PLUGIN_PROCESS_MARKER";

/// Child-side behavior for the runner test; does nothing in a normal run.
#[test]
fn process_fixture() {
    match std::env::var(PROCESS_FIXTURE).as_deref() {
        Ok("sleep") => std::thread::sleep(Duration::from_secs(30)),
        Ok("flood") => {
            let chunk = [b'x'; 65_536];
            let mut stdout = std::io::stdout().lock();
            while stdout.write_all(&chunk).is_ok() {}
        }
        Ok("inherit") => {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "plugin_management::tests::process_fixture",
                    "--nocapture",
                ])
                .env(PROCESS_FIXTURE, "descendant")
                .spawn()
                .unwrap();
        }
        Ok("descendant") => {
            let deadline = Instant::now() + Duration::from_secs(4);
            let mut stderr = std::io::stderr().lock();
            let outcome = loop {
                if stderr.write_all(b"still running\n").is_err() {
                    break "closed";
                }
                if Instant::now() >= deadline {
                    break "deadline";
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            fs::write(std::env::var_os(PROCESS_MARKER).unwrap(), outcome).unwrap();
        }
        _ => {}
    }
}

fn fixture_run(mode: &str, limits: Limits, cancel: &AtomicBool) -> Result<Output, RunError> {
    let program = std::env::current_exe().unwrap();
    let arguments: [&OsStr; 3] = [
        "--exact".as_ref(),
        "plugin_management::tests::process_fixture".as_ref(),
        "--nocapture".as_ref(),
    ];
    process::run(
        &program,
        &arguments,
        &std::env::temp_dir(),
        &[(PROCESS_FIXTURE, mode.into())],
        limits,
        cancel,
    )
}

fn started(result: Result<Output, RunError>) -> String {
    match result {
        Err(RunError::Started(fault)) => fault.category,
        Err(RunError::NotStarted(fault)) => panic!("unexpected start failure {fault:?}"),
        Ok(_) => panic!("the bounded run must settle its child early"),
    }
}

#[test]
fn the_client_runner_settles_its_own_child_at_every_bound() {
    let limits = Limits {
        duration: Duration::from_secs(60),
        stdout: 1024 * 1024,
        stderr: 64 * 1024,
    };
    let Ok(done) = fixture_run("none", limits, &AtomicBool::new(false)) else {
        panic!("the fixture child must complete");
    };
    assert!(done.success);
    let begun = Instant::now();
    let short = Limits {
        duration: Duration::from_millis(300),
        ..limits
    };
    assert_eq!(
        started(fixture_run("sleep", short, &AtomicBool::new(false))),
        "PluginTimeout"
    );
    assert!(begun.elapsed() < Duration::from_secs(10));
    let small = Limits {
        stdout: 1024,
        ..limits
    };
    assert_eq!(
        started(fixture_run("flood", small, &AtomicBool::new(false))),
        "PluginOutputLimit"
    );
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(200));
            cancel.store(true, Ordering::Release);
        });
        assert_eq!(
            started(fixture_run("sleep", limits, &cancel)),
            "PluginInterrupted"
        );
    });
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
fn inherited_output_is_closed_when_the_owned_command_settles() {
    let root = Root::new();
    let marker = root.0.join("pipe-closed");
    let limits = Limits {
        duration: Duration::from_secs(10),
        stdout: 1024 * 1024,
        stderr: 64 * 1024,
    };
    let result = process::run(
        &std::env::current_exe().unwrap(),
        &[
            "--exact".as_ref(),
            "plugin_management::tests::process_fixture".as_ref(),
            "--nocapture".as_ref(),
        ],
        &root.0,
        &[
            (PROCESS_FIXTURE, "inherit".into()),
            (PROCESS_MARKER, marker.clone().into()),
        ],
        limits,
        &AtomicBool::new(false),
    );
    assert_eq!(started(result), "PluginProcess");
    let deadline = Instant::now() + Duration::from_secs(6);
    while !marker.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fs::read_to_string(marker).unwrap(), "closed");
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
