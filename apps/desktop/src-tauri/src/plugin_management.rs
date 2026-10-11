//! Application-managed installation of the MadoMata OMP adapter.
//!
//! The build supplies one finite adapter payload. Install, Update and Migrate
//! prepare an app-owned copy under its version/content identity, then register it
//! in the normal per-user OMP installation through fixed client commands; Uninstall
//! removes only that registration. An action is admitted only against the
//! observation the operator reviewed, and is rechecked against current facts
//! before dispatch. Success requires readback of the expected registration,
//! version and source. A dispatched mutation without such readback is `unknown`,
//! never a claimed rollback or a replay. No workspace or Edit admission is held,
//! and no OMP session is touched.

mod payload;
mod process;
mod target;
#[cfg(all(test, unix))]
mod tests;
mod version;

use mado_runtime_comparison::model::Fault;
use payload::{Included, NAME, PLUGINS};
use process::{Cli, Client, Invocation, Operation, Output, RunError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::cmp::Ordering as Order;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};
use target::{Environment, Executable, OMP_DIRECTORY, display};
use version::Version;

/// After the shutdown drain, an interrupted child must settle within this bound.
const SETTLE: Duration = Duration::from_secs(2);
/// Records a dispatched mutation until its result is observed.
const JOURNAL: &str = "omp-operation.json";
const MAX_JOURNAL_BYTES: usize = 16 * 1024;
const MAX_DIAGNOSTIC_BYTES: usize = 2048;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginAction {
    Install,
    Update,
    Uninstall,
    Migrate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    /// No usable OMP executable was found or selected.
    Missing,
    /// The platform, environment, target or inventory cannot be established.
    Unsupported,
    /// OMP is older than the minimum or lacks a required management command.
    Incompatible,
    Absent,
    Current,
    UpdateAvailable,
    /// A recognized predecessor checkout link that may be migrated explicitly.
    Legacy,
    Conflict,
}

/// The established OMP target; installation scope is always the normal user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginTarget {
    pub executable: String,
    pub version: String,
    pub user_root: String,
}

/// The registered adapter; `source` is the canonical registered directory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PluginInstallation {
    pub version: String,
    pub source: String,
    pub content: Option<String>,
    pub managed: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeStatus {
    /// Readback shows the intended registration.
    Verified,
    /// The mutation may have run; the shown state is the only known result.
    Unknown,
    /// Nothing was applied, or the installation was observed unchanged.
    Failed,
}

#[derive(Clone, Debug, Serialize)]
pub struct PluginOutcome {
    pub action: PluginAction,
    pub target: Option<PluginTarget>,
    pub status: OutcomeStatus,
    pub issue: Option<Fault>,
}

#[derive(Debug, Serialize)]
pub struct PluginView {
    pub included_version: String,
    pub included_content: String,
    pub minimum_omp_version: String,
    pub target: Option<PluginTarget>,
    pub installed: Option<PluginInstallation>,
    pub state: PluginState,
    pub actions: Vec<PluginAction>,
    /// Admits exactly one `plugin_apply`; present only when actions are available.
    pub observation: Option<String>,
    pub issue: Option<Fault>,
    pub outcome: Option<PluginOutcome>,
}

/// Facts an action is admitted against; any difference at recheck refuses it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Facts {
    executable: Option<Executable>,
    target: Option<PluginTarget>,
    installed: Option<PluginInstallation>,
    link: Option<PathBuf>,
    state: PluginState,
    actions: Vec<PluginAction>,
}

struct Observation {
    facts: Facts,
    issue: Option<Fault>,
}

impl Observation {
    fn with(facts: Facts, issue: Option<Fault>) -> Self {
        Self { facts, issue }
    }

    fn refused(state: PluginState, issue: Fault) -> Self {
        Self::with(
            Facts {
                executable: None,
                target: None,
                installed: None,
                link: None,
                state,
                actions: Vec::new(),
            },
            Some(issue),
        )
    }
}

#[derive(Clone, Copy)]
enum Selection<'a> {
    Discover,
    Explicit(&'a str),
    Captured(&'a Path),
}

struct Reviewed {
    token: String,
    facts: Facts,
}

/// Version and capability facts reused while the executable keeps its file identity.
struct Probe {
    executable: Executable,
    version: Version,
    capable: bool,
}

#[derive(Default)]
struct State {
    closing: bool,
    busy: bool,
    reviewed: Option<Reviewed>,
    last: Option<PluginOutcome>,
    journal_checked: bool,
}

#[derive(Serialize, Deserialize)]
struct Journal {
    action: PluginAction,
    target: PluginTarget,
}

pub struct PluginManager {
    root: Result<PathBuf, Fault>,
    environment: Environment,
    platform: bool,
    client: Box<dyn Client>,
    probe: Mutex<Option<Probe>>,
    state: Mutex<State>,
    settled: Condvar,
    cancel: AtomicBool,
}

/// Clears the mutation slot if an admitted operation unwinds.
struct Busy<'a> {
    manager: &'a PluginManager,
    armed: bool,
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        if self.armed {
            lock(&self.manager.state).busy = false;
            self.manager.settled.notify_all();
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn closing() -> Fault {
    Fault::new("Closing", "Application is closing")
}

fn admit(state: &State) -> Result<(), Fault> {
    if state.closing {
        return Err(closing());
    }
    if state.busy {
        return Err(Fault::new(
            "PluginBusy",
            "A plugin operation is still in progress; its result is reported when it settles",
        ));
    }
    Ok(())
}

fn publish(state: &mut State, facts: &Facts) -> Option<String> {
    state.reviewed = None;
    if facts.actions.is_empty() {
        return None;
    }
    let token = crate::storage::new_id().ok()?;
    state.reviewed = Some(Reviewed {
        token: token.clone(),
        facts: facts.clone(),
    });
    Some(token)
}

fn journal_path(root: &Path) -> PathBuf {
    root.join(PLUGINS).join(JOURNAL)
}

fn io_fault(category: &str, message: &str, error: &io::Error) -> Fault {
    Fault::new(category, message).with_context(
        json!({"kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}),
    )
}

fn diagnostic(bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_DIAGNOSTIC_BYTES)]).into_owned()
}

impl PluginManager {
    /// `root` is the configured data root; `home` locates the user's OMP installation.
    pub fn new(root: Result<PathBuf, Fault>, home: Result<PathBuf, Fault>) -> Self {
        Self::with_client(
            root,
            Environment::capture(home),
            cfg!(target_os = "macos"),
            Box::new(Cli),
        )
    }

    fn with_client(
        root: Result<PathBuf, Fault>,
        environment: Environment,
        platform: bool,
        client: Box<dyn Client>,
    ) -> Self {
        Self {
            root,
            environment,
            platform,
            client,
            probe: Mutex::new(None),
            state: Mutex::new(State::default()),
            settled: Condvar::new(),
            cancel: AtomicBool::new(false),
        }
    }

    /// Observes the discovered (`None`) or selected OMP target and the adapter
    /// registration. A view with actions carries a fresh one-use observation and
    /// invalidates every earlier one.
    ///
    /// # Errors
    ///
    /// `Closing`, `PluginBusy` while a mutation is pending, or `PluginPayload`
    /// when the build's adapter payload is invalid. Target problems are reported
    /// in the view, not as errors.
    pub fn inspect(&self, executable: Option<&str>) -> Result<PluginView, Fault> {
        let included = payload::included()?;
        {
            let mut state = lock(&self.state);
            admit(&state)?;
            if !state.journal_checked {
                state.journal_checked = true;
                if let Some(outcome) = self.recover() {
                    state.last = Some(outcome);
                }
            }
        }
        let selection = executable.map_or(Selection::Discover, Selection::Explicit);
        let observed = self.observe(selection, included);
        let mut state = lock(&self.state);
        admit(&state)?;
        let token = publish(&mut state, &observed.facts);
        let last = state.last.clone();
        drop(state);
        Ok(view(included, &observed, token, last))
    }

    /// Applies `action` to the reviewed observation, which is consumed by any call.
    /// Every admitted action returns an outcome and the current readback.
    ///
    /// # Errors
    ///
    /// `Closing`, `PluginBusy`, `PluginStale` for an unknown or superseded
    /// observation, `PluginActionUnavailable`, or `PluginPayload`. Nothing is
    /// dispatched for these refusals.
    pub fn apply(&self, observation: &str, action: PluginAction) -> Result<PluginView, Fault> {
        let included = payload::included()?;
        let admitted = {
            let mut state = lock(&self.state);
            admit(&state)?;
            let reviewed = state
                .reviewed
                .take()
                .filter(|reviewed| reviewed.token == observation)
                .ok_or_else(|| {
                    Fault::new(
                        "PluginStale",
                        "This plugin review is no longer current; inspect the target again",
                    )
                })?;
            if !reviewed.facts.actions.contains(&action) {
                return Err(Fault::new(
                    "PluginActionUnavailable",
                    "This action is not available for the reviewed installation",
                )
                .with_context(json!({"action": action, "state": reviewed.facts.state})));
            }
            state.busy = true;
            reviewed.facts
        };
        let mut busy = Busy {
            manager: self,
            armed: true,
        };
        let (observed, outcome) = self.execute(included, &admitted, action);
        let mut state = lock(&self.state);
        state.busy = false;
        busy.armed = false;
        self.settled.notify_all();
        state.last = Some(outcome.clone());
        let token = if state.closing {
            state.reviewed = None;
            None
        } else {
            publish(&mut state, &observed.facts)
        };
        drop(state);
        Ok(view(included, &observed, token, Some(outcome)))
    }

    /// Refuses new work, lets an admitted mutation settle for up to `drain`, then
    /// interrupts this application's own client child. An interrupted mutation
    /// keeps its journal, so the next inspection reports an attributable unknown
    /// outcome. Never signals an OMP session. Idempotent.
    pub fn shutdown(&self, drain: Duration) {
        let mut state = lock(&self.state);
        state.closing = true;
        state.reviewed = None;
        state = self.idle(state, Instant::now() + drain);
        self.cancel.store(true, Ordering::Release);
        drop(self.idle(state, Instant::now() + SETTLE));
    }

    fn idle<'a>(
        &self,
        mut state: MutexGuard<'a, State>,
        deadline: Instant,
    ) -> MutexGuard<'a, State> {
        while state.busy {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            state = self
                .settled
                .wait_timeout(state, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        state
    }

    fn is_closing(&self) -> bool {
        lock(&self.state).closing
    }

    fn execute(
        &self,
        included: &Included,
        admitted: &Facts,
        action: PluginAction,
    ) -> (Observation, PluginOutcome) {
        let outcome = |status, issue| PluginOutcome {
            action,
            target: admitted.target.clone(),
            status,
            issue: Some(issue),
        };
        let (Some(executable), Some(target), Ok(root), Ok(home)) = (
            admitted.executable.clone(),
            admitted.target.clone(),
            self.root.as_ref(),
            self.environment.home.as_ref(),
        ) else {
            let fault = Fault::new(
                "PluginStale",
                "The reviewed observation has no established target",
            );
            return (
                Observation::refused(PluginState::Unsupported, fault.clone()),
                outcome(OutcomeStatus::Failed, fault),
            );
        };
        let selection = Selection::Captured(&executable.path);
        // Bytes are prepared and verified before any registration changes.
        let prepared = match action {
            PluginAction::Uninstall => None,
            _ => match payload::prepare(root, included) {
                Ok(path) => Some(path),
                Err(fault) => {
                    return (
                        self.observe(selection, included),
                        outcome(OutcomeStatus::Failed, fault),
                    );
                }
            },
        };
        let current = self.observe(selection, included);
        if current.facts != *admitted {
            return (
                current,
                outcome(
                    OutcomeStatus::Failed,
                    Fault::new(
                        "PluginStale",
                        "The OMP target or adapter installation changed after review; nothing was changed",
                    ),
                ),
            );
        }
        if self.is_closing() {
            return (
                Observation::refused(PluginState::Unsupported, closing()),
                outcome(OutcomeStatus::Failed, closing()),
            );
        }
        let journal = Journal { action, target };
        if let Err(fault) = write_journal(root, &journal) {
            return (current, outcome(OutcomeStatus::Failed, fault));
        }
        let environment = self.environment.child(home, &executable.path);
        let invocation = Invocation {
            executable: &executable.path,
            home,
            environment: &environment,
            cancel: &self.cancel,
        };
        let operation = prepared
            .as_deref()
            .map_or(Operation::Uninstall, Operation::Link);
        let dispatched = self.client.run(&invocation, operation);
        let readback = (!self.cancel.load(Ordering::Acquire))
            .then(|| self.observe(selection, included))
            .filter(|_| !self.cancel.load(Ordering::Acquire));
        let Some(readback) = readback else {
            let refused = Observation::refused(PluginState::Unsupported, closing());
            let issue = match dispatched {
                Err(RunError::NotStarted(fault)) => {
                    let _ = fs::remove_file(journal_path(root));
                    return (refused, outcome(OutcomeStatus::Failed, fault));
                }
                Err(RunError::Started(fault)) => fault,
                Ok(_) => Fault::new(
                    "PluginInterrupted",
                    "Application shutdown began before the OMP result could be read back",
                ),
            };
            // The journal stays: the result is left for the next inspection.
            return (refused, outcome(OutcomeStatus::Unknown, issue));
        };
        let _ = fs::remove_file(journal_path(root));
        let (status, issue) = settle(admitted, &readback, prepared.as_deref(), dispatched);
        (
            readback,
            PluginOutcome {
                action,
                target: admitted.target.clone(),
                status,
                issue,
            },
        )
    }

    /// Reports a mutation journaled by an earlier run that never observed its result.
    fn recover(&self) -> Option<PluginOutcome> {
        let path = journal_path(self.root.as_ref().ok()?);
        if !crate::storage::exists(&path).ok()? {
            return None;
        }
        let journal: Journal = crate::storage::read_bytes(&path, MAX_JOURNAL_BYTES)
            .and_then(|bytes| crate::storage::decode(&bytes))
            .ok()?;
        let _ = fs::remove_file(&path);
        Some(PluginOutcome {
            action: journal.action,
            status: OutcomeStatus::Unknown,
            issue: Some(
                Fault::new(
                    "PluginInterrupted",
                    "An earlier plugin operation stopped before its result was observed; the installation below is the current readback",
                )
                .with_context(json!({"executable": journal.target.executable, "user_root": journal.target.user_root})),
            ),
            target: Some(journal.target),
        })
    }

    fn observe(&self, selection: Selection<'_>, included: &Included) -> Observation {
        if !self.platform {
            return Observation::refused(
                PluginState::Unsupported,
                Fault::new(
                    "PluginPlatform",
                    "Managed OMP installation is available only in the macOS desktop",
                ),
            );
        }
        let root = match &self.root {
            Ok(root) => root,
            Err(fault) => return Observation::refused(PluginState::Unsupported, fault.clone()),
        };
        let home = match &self.environment.home {
            Ok(home) => home,
            Err(fault) => {
                return Observation::refused(
                    PluginState::Unsupported,
                    Fault::new("PluginHome", "The user home directory is unavailable")
                        .with_context(json!({"cause": fault})),
                );
            }
        };
        let overrides = self.environment.overrides(home);
        if !overrides.is_empty() {
            return Observation::refused(
                PluginState::Unsupported,
                Fault::new(
                    "PluginTargetOverride",
                    "OMP profile or agent-state overrides are set; only the normal user installation is managed",
                )
                .with_context(json!({"variables": overrides})),
            );
        }
        let resolved = match selection {
            Selection::Discover => target::discover(&self.environment.search),
            Selection::Explicit(text) => target::explicit(text),
            Selection::Captured(path) => target::selected(path),
        };
        let executable = match resolved {
            Ok(executable) => executable,
            Err((state, fault)) => return Observation::refused(state, fault),
        };
        let environment = self.environment.child(home, &executable.path);
        let invocation = Invocation {
            executable: &executable.path,
            home,
            environment: &environment,
            cancel: &self.cancel,
        };
        let Some((version, capable)) = self.probe(&executable, &invocation) else {
            return Observation::refused(
                PluginState::Unsupported,
                Fault::new(
                    "PluginVersionUnavailable",
                    "The selected executable did not report an OMP version",
                )
                .with_context(json!({"executable": display(&executable.path)})),
            );
        };
        let user_root = home.join(OMP_DIRECTORY);
        let mut facts = Facts {
            target: Some(PluginTarget {
                executable: display(&executable.path),
                version: version.to_string(),
                user_root: display(&user_root),
            }),
            executable: Some(executable.clone()),
            installed: None,
            link: None,
            state: PluginState::Incompatible,
            actions: Vec::new(),
        };
        if version < included.minimum {
            return Observation::with(
                facts,
                Some(
                    Fault::new(
                        "PluginVersionTooOld",
                        format!("MadoMata requires OMP >={}", included.minimum),
                    )
                    .with_context(json!({"minimum": included.minimum.to_string(), "found": version.to_string()})),
                ),
            );
        }
        if !capable {
            return Observation::with(
                facts,
                Some(Fault::new(
                    "PluginCapabilityMissing",
                    "The selected OMP does not provide plugin link, list, uninstall and JSON output",
                )),
            );
        }
        let listed = match self.client.run(&invocation, Operation::List) {
            Ok(output) if output.success => target::inventory(&output.stdout),
            Ok(output) => Err(Fault::new(
                "PluginInventoryUnavailable",
                "OMP could not list installed plugins",
            )
            .with_context(json!({"code": output.code, "stderr": diagnostic(&output.stderr)}))),
            Err(error) => Err(error.fault()),
        };
        match listed {
            Ok(inventory) => classify(facts, &inventory, root, &user_root, included),
            Err(fault) => {
                facts.state = PluginState::Unsupported;
                Observation::with(facts, Some(fault))
            }
        }
    }

    fn probe(
        &self,
        executable: &Executable,
        invocation: &Invocation<'_>,
    ) -> Option<(Version, bool)> {
        if let Some(probe) = lock(&self.probe)
            .as_ref()
            .filter(|probe| probe.executable == *executable)
        {
            return Some((probe.version.clone(), probe.capable));
        }
        let version = match self.client.run(invocation, Operation::Version) {
            Ok(output) if output.success => target::omp_version(&output.stdout)?,
            _ => return None,
        };
        let help = match self.client.run(invocation, Operation::Help) {
            Ok(output) => Some(
                output.success
                    && (target::capable(&output.stdout) || target::capable(&output.stderr)),
            ),
            Err(_) => None,
        };
        // Only a completed probe is a fact about this file.
        if let Some(capable) = help {
            *lock(&self.probe) = Some(Probe {
                executable: executable.clone(),
                version: version.clone(),
                capable,
            });
        }
        Some((version, help.unwrap_or(false)))
    }
}

fn write_journal(root: &Path, journal: &Journal) -> Result<(), Fault> {
    crate::storage::private_directory(&root.join(PLUGINS))?;
    let bytes = crate::storage::encode(journal, MAX_JOURNAL_BYTES)?;
    crate::storage::write_atomic(&journal_path(root), &bytes, |from, to| fs::rename(from, to))
}

fn view(
    included: &Included,
    observed: &Observation,
    observation: Option<String>,
    outcome: Option<PluginOutcome>,
) -> PluginView {
    PluginView {
        included_version: included.version.to_string(),
        included_content: included.content.clone(),
        minimum_omp_version: included.minimum.to_string(),
        target: observed.facts.target.clone(),
        installed: observed.facts.installed.clone(),
        state: observed.facts.state,
        actions: if observation.is_some() {
            observed.facts.actions.clone()
        } else {
            Vec::new()
        },
        observation,
        issue: observed.issue.clone(),
        outcome,
    }
}

/// Derives an outcome only from the child result and the target readback.
fn settle(
    admitted: &Facts,
    readback: &Observation,
    prepared: Option<&Path>,
    dispatched: Result<Output, RunError>,
) -> (OutcomeStatus, Option<Fault>) {
    let facts = &readback.facts;
    let intended = match prepared {
        None => facts.state == PluginState::Absent,
        Some(source) => {
            facts.state == PluginState::Current
                && facts.installed.as_ref().is_some_and(|installed| {
                    installed.managed && installed.source == display(source)
                })
        }
    };
    let unchanged = facts == admitted;
    match dispatched {
        Err(RunError::NotStarted(fault)) => (OutcomeStatus::Failed, Some(fault)),
        Err(RunError::Started(mut fault)) => {
            fault.context["readback"] = json!(facts.state);
            fault.context["intended"] = json!(intended);
            (OutcomeStatus::Unknown, Some(fault))
        }
        Ok(output) if output.success && intended => (OutcomeStatus::Verified, None),
        Ok(output) if output.success => (
            OutcomeStatus::Unknown,
            Some(
                Fault::new(
                    "PluginReadback",
                    "OMP reported success, but readback did not show the expected registration",
                )
                .with_context(json!({
                    "readback": facts.state,
                    "issue": readback.issue,
                    "receipt": receipt(prepared, &output.stdout),
                })),
            ),
        ),
        Ok(output) => {
            let (status, message) = if unchanged {
                (
                    OutcomeStatus::Failed,
                    "OMP refused the operation; the installation was observed unchanged",
                )
            } else {
                (
                    OutcomeStatus::Unknown,
                    "OMP reported failure after the installation changed; review the current state",
                )
            };
            (
                status,
                Some(
                    Fault::new("PluginCommandFailed", message).with_context(json!({
                        "code": output.code,
                        "stderr": diagnostic(&output.stderr),
                        "readback": facts.state,
                    })),
                ),
            )
        }
    }
}

/// Whether the client's JSON receipt names the intended change; diagnostic only.
fn receipt(prepared: Option<&Path>, stdout: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(stdout) else {
        return false;
    };
    let text = |key: &str| value.get(key).and_then(Value::as_str);
    match prepared {
        Some(source) => text("name") == Some(NAME) && text("path") == source.to_str(),
        None => text("uninstalled") == Some(NAME),
    }
}

type Classified = (PluginState, Vec<PluginAction>, Option<Fault>);

fn classify(
    mut facts: Facts,
    inventory: &target::Inventory,
    root: &Path,
    user_root: &Path,
    included: &Included,
) -> Observation {
    let registration = user_root.join("plugins").join("node_modules").join(NAME);
    let (state, actions, issue) = registered(&mut facts, inventory, root, &registration, included)
        .unwrap_or_else(|fault| (PluginState::Unsupported, Vec::new(), Some(fault)));
    let (state, actions, issue) = if inventory.collision {
        (
            PluginState::Conflict,
            Vec::new(),
            Some(Fault::new(
                "PluginInventoryCollision",
                "Another OMP plugin entry uses the adapter name; management is refused because removal could select it",
            )),
        )
    } else {
        (state, actions, issue)
    };
    facts.state = state;
    facts.actions = actions;
    Observation::with(facts, issue)
}

fn registered(
    facts: &mut Facts,
    inventory: &target::Inventory,
    root: &Path,
    registration: &Path,
    included: &Included,
) -> Result<Classified, Fault> {
    use PluginAction::{Install, Migrate, Uninstall, Update};
    use PluginState::{Absent, Conflict, Current, Legacy, UpdateAvailable};
    let unexpected = |message: &str| {
        Fault::new("PluginRegistrationUnexpected", message)
            .with_context(json!({"registration": display(registration)}))
    };
    let metadata = match fs::symlink_metadata(registration) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(io_fault(
                "PluginInventoryUnavailable",
                "The adapter registration cannot be inspected",
                &error,
            ));
        }
    };
    let (entry, metadata) = match (&inventory.entry, metadata) {
        (None, None) => return Ok((Absent, vec![Install], None)),
        (None, Some(_)) => {
            return Ok((
                Conflict,
                Vec::new(),
                Some(unexpected("A registration exists that OMP does not list")),
            ));
        }
        (Some(_), None) => {
            return Ok((
                Conflict,
                Vec::new(),
                Some(unexpected(
                    "OMP lists the adapter without its user registration",
                )),
            ));
        }
        (Some(entry), Some(metadata)) => (entry, metadata),
    };
    if entry.path.as_deref() != Some(display(registration).as_str()) {
        return Ok((
            Conflict,
            Vec::new(),
            Some(unexpected(
                "OMP lists the adapter outside the normal user registration",
            )),
        ));
    }
    let listed = entry.version.clone().unwrap_or_default();
    if !metadata.file_type().is_symlink() {
        facts.installed = Some(PluginInstallation {
            version: listed,
            source: display(registration),
            content: None,
            managed: false,
        });
        return Ok((
            Conflict,
            Vec::new(),
            Some(Fault::new(
                "PluginRegistrationNotLink",
                "The registration is a copied package; replacing or removing it would delete its files",
            )),
        ));
    }
    let link = fs::read_link(registration).map_err(|error| {
        io_fault(
            "PluginInventoryUnavailable",
            "The adapter registration cannot be read",
            &error,
        )
    })?;
    let linked = registration
        .parent()
        .map_or_else(|| link.clone(), |parent| parent.join(&link));
    facts.link = Some(link);
    let source = match registration.canonicalize() {
        Ok(source) if source.is_dir() => source,
        _ => {
            facts.installed = Some(PluginInstallation {
                version: listed,
                source: display(&linked),
                content: None,
                managed: false,
            });
            return Ok((
                Conflict,
                vec![Uninstall],
                Some(Fault::new(
                    "PluginSourceUnavailable",
                    "The registered adapter source is missing or not a directory",
                )),
            ));
        }
    };
    let inventory_mismatch = |version: &Version| {
        entry
            .version
            .as_deref()
            .is_some_and(|listed| Version::parse(listed).as_ref() != Some(version))
    };
    let base = payload::base(root).canonicalize().ok();
    if base.is_some() && source.parent() == base.as_deref() {
        let managed = match payload::managed(&source) {
            Ok(managed) => managed,
            Err(fault) => {
                let found = payload::source(&source, included);
                facts.installed = Some(PluginInstallation {
                    version: found.version.unwrap_or(listed),
                    source: display(&source),
                    content: found.content,
                    managed: false,
                });
                return Ok((
                    Conflict,
                    vec![Uninstall],
                    Some(
                        Fault::new(
                            "PluginPayloadIntegrity",
                            "The registered application payload does not match its identity",
                        )
                        .with_context(json!({"cause": fault})),
                    ),
                ));
            }
        };
        facts.installed = Some(PluginInstallation {
            version: managed.version.to_string(),
            source: display(&source),
            content: Some(managed.content.clone()),
            managed: true,
        });
        if inventory_mismatch(&managed.version) {
            return Ok((
                Conflict,
                vec![Uninstall],
                Some(unexpected(
                    "OMP lists a different version than the registered source",
                )),
            ));
        }
        return Ok(match managed.version.cmp(&included.version) {
            Order::Less => (UpdateAvailable, vec![Update, Uninstall], None),
            Order::Equal if managed.content == included.content => (Current, vec![Uninstall], None),
            Order::Equal => (Conflict, vec![Uninstall], Some(content_mismatch())),
            Order::Greater => (Conflict, vec![Uninstall], Some(newer())),
        });
    }
    let found = payload::source(&source, included);
    let version = found.version.as_deref().and_then(Version::parse);
    facts.installed = Some(PluginInstallation {
        version: found.version.clone().unwrap_or(listed),
        source: display(&source),
        content: found.content.clone(),
        managed: false,
    });
    if version.as_ref().is_some_and(inventory_mismatch) {
        return Ok((
            Conflict,
            vec![Uninstall],
            Some(unexpected(
                "OMP lists a different version than the registered source",
            )),
        ));
    }
    if found.recognized {
        return Ok((Legacy, vec![Migrate, Uninstall], None));
    }
    let named = found.name.as_deref() == Some(NAME);
    Ok(match version {
        Some(version) if named && version > included.version => {
            (Conflict, vec![Uninstall], Some(newer()))
        }
        Some(version) if named && version == included.version => {
            (Conflict, vec![Uninstall], Some(content_mismatch()))
        }
        _ => (
            Conflict,
            vec![Uninstall],
            Some(Fault::new(
                "PluginUnrecognizedSource",
                "The registered source is not a recognized MadoMata adapter release; it is not adopted",
            )),
        ),
    })
}

fn newer() -> Fault {
    Fault::new(
        "PluginNewerInstalled",
        "A newer adapter is installed; it is never downgraded automatically",
    )
}

fn content_mismatch() -> Fault {
    Fault::new(
        "PluginContentMismatch",
        "The installed adapter has the included version with different content",
    )
}
