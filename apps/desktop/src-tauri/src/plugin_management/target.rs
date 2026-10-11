//! The normal per-user OMP target: environment, executable, inventory and
//! installation ownership facts.

use super::PluginState;
use super::payload::NAME;
use super::version::Version;
use mado_runtime_comparison::model::Fault;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

/// OMP's per-user state directory below HOME.
pub(super) const OMP_DIRECTORY: &str = ".omp";
const EXECUTABLE: &str = "omp";
const MAX_PATH_BYTES: usize = 4096;
const MAX_SEARCH: usize = 64;
const MAX_INVENTORY: usize = 4096;
const DEFAULT_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";
/// Variables that select profile, agent-state or plugin-data locations for OMP.
const OVERRIDES: [&str; 5] = [
    "OMP_PROFILE",
    "PI_PROFILE",
    "PI_CODING_AGENT_DIR",
    "PI_CONFIG_DIR",
    "XDG_DATA_HOME",
];
/// The same selections in the client's dotenv files, where OMP also mirrors
/// `OMP_*` names to their `PI_*` aliases.
const DOTENV_OVERRIDES: [&str; 7] = [
    "OMP_PROFILE",
    "PI_PROFILE",
    "PI_CODING_AGENT_DIR",
    "OMP_CODING_AGENT_DIR",
    "PI_CONFIG_DIR",
    "OMP_CONFIG_DIR",
    "XDG_DATA_HOME",
];
const MAX_DOTENV_BYTES: u64 = 256 * 1024;
/// Below the user root: a management command for this installation is pending
/// or could not be confirmed to have ended. Only its own settled command or
/// the operator removes it.
const UNSETTLED: &str = "mado-mata-plugin-unsettled.json";
const MAX_MARKER_BYTES: u64 = 16 * 1024;

pub(super) type Refusal = (PluginState, Fault);

pub(super) fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Whether `name=value` makes OMP use state other than the normal user
/// installation below `home`. Relative paths resolve against `directory`, the
/// client's working directory, when it is known.
fn redirects(name: &str, value: &OsStr, home: &Path, directory: Option<&Path>) -> bool {
    let text = value.to_string_lossy();
    let text = text.trim();
    let path = || directory.map_or_else(|| PathBuf::from(value), |directory| directory.join(value));
    !text.is_empty()
        && match name {
            "OMP_PROFILE" | "PI_PROFILE" => text != "default",
            "PI_CODING_AGENT_DIR" | "OMP_CODING_AGENT_DIR" => {
                path() != home.join(OMP_DIRECTORY).join("agent")
            }
            "PI_CONFIG_DIR" | "OMP_CONFIG_DIR" => text != OMP_DIRECTORY,
            // OMP prefers an existing `$XDG_DATA_HOME/omp` for plugin data.
            _ => path().join(EXECUTABLE).exists(),
        }
}

/// Dotenv files the client loads with HOME as its working directory: Bun's
/// autoloaded working-directory files, then OMP's agent and config-root files.
fn dotenv_files(home: &Path) -> [PathBuf; 6] {
    let user_root = home.join(OMP_DIRECTORY);
    [
        home.join(".env.development.local"),
        home.join(".env.local"),
        home.join(".env.development"),
        home.join(".env"),
        user_root.join("agent").join(".env"),
        user_root.join(".env"),
    ]
}

/// A dotenv definition of one of `DOTENV_OVERRIDES`, or `None` for any other
/// line. An expanded, escaped, commented or unterminated quoted value is
/// `Some((name, None))`: it cannot be shown to keep the normal target.
fn definition(line: &str) -> Option<(&'static str, Option<&str>)> {
    let line = line.trim_start();
    let line = line
        .strip_prefix("export")
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map_or(line, str::trim_start);
    let end = line
        .find(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-')))
        .unwrap_or(line.len());
    let name = DOTENV_OVERRIDES
        .into_iter()
        .find(|name| *name == &line[..end])?;
    let rest = line[end..].trim_start();
    let rest = rest
        .strip_prefix('=')
        .or_else(|| rest.strip_prefix(':'))?
        .trim();
    let value = match rest.chars().next() {
        Some(quote @ ('"' | '\'' | '`')) => rest[1..].find(quote).map(|close| &rest[1..=close]),
        _ => Some(rest).filter(|value| !value.contains('#')),
    };
    Some((name, value.filter(|value| !value.contains(['$', '\\']))))
}

/// Reads one dotenv file as the client would; an absent file or a directory
/// is not loaded.
fn dotenv_file(path: &Path) -> Result<Option<String>, Fault> {
    let unverified = |context: Value| {
        Fault::new(
            "PluginTargetUnverified",
            "An OMP environment file cannot be checked for profile or agent-state overrides; only the normal user installation is managed",
        )
        .with_context(context)
    };
    let failed = |error: &io::Error| {
        unverified(json!({
            "file": display(path),
            "kind": format!("{:?}", error.kind()),
            "os_code": error.raw_os_error(),
        }))
    };
    let oversized = || unverified(json!({"file": display(path), "limit": MAX_DOTENV_BYTES}));
    match fs::metadata(path) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(failed(&error)),
        Ok(metadata) if metadata.is_dir() => return Ok(None),
        // A FIFO or device is never opened: reading it may block or never end.
        Ok(metadata) if !metadata.is_file() => {
            return Err(unverified(json!({"file": display(path)})));
        }
        Ok(metadata) if metadata.len() > MAX_DOTENV_BYTES => return Err(oversized()),
        Ok(_) => {}
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_DOTENV_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| failed(&error))?;
    if bytes.len() as u64 > MAX_DOTENV_BYTES {
        return Err(oversized());
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

/// Directory-selection directives in the dotenv files the supported client
/// loads for the normal user target. A redirecting definition is reported
/// whichever file would take precedence; nothing is cleared or overridden.
///
/// # Errors
///
/// `PluginTargetUnverified` when such a file cannot be read within its bound.
pub(super) fn dotenv(home: &Path) -> Result<Vec<Value>, Fault> {
    let mut found = Vec::new();
    for path in dotenv_files(home) {
        let Some(text) = dotenv_file(&path)? else {
            continue;
        };
        for (name, value) in text.lines().filter_map(definition) {
            let entry = json!({"file": display(&path), "variable": name});
            if value.is_none_or(|value| redirects(name, OsStr::new(value), home, Some(home)))
                && !found.contains(&entry)
            {
                found.push(entry);
            }
        }
    }
    Ok(found)
}

/// Takes the advisory lock on the user root below `home`, the canonical
/// directory every HOME and data root of this installation resolves to. The
/// directory is created only when missing, as the client's own registration
/// would. `None` while another owner holds it.
fn own(home: &Path) -> io::Result<Option<File>> {
    let user_root = home.join(OMP_DIRECTORY);
    if let Err(error) = fs::create_dir(&user_root)
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error);
    }
    let file = File::open(&user_root)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

fn busy() -> Fault {
    Fault::new(
        "PluginInstallationBusy",
        "Another MadoMata instance is using this OMP user installation; Refresh after it finishes",
    )
}

/// Exclusive ownership of the normal user installation, shared by every
/// application instance, data root and HOME whose user root resolves to the
/// same directory. It is released when the handle is dropped or the process
/// ends; a client child never inherits it.
///
/// # Errors
///
/// `PluginInstallationBusy` while another owner holds it, or
/// `PluginInstallationLock` when ownership cannot be established.
pub(super) fn lease(home: &Path) -> Result<File, Fault> {
    match own(home) {
        Ok(Some(file)) => Ok(file),
        Ok(None) => Err(busy()),
        Err(error) => Err(Fault::new(
            "PluginInstallationLock",
            "Ownership of the OMP user installation could not be established; nothing was changed",
        )
        .with_context(
            json!({"kind": format!("{:?}", error.kind()), "os_code": error.raw_os_error()}),
        )),
    }
}

fn unsettled_marker(home: &Path) -> PathBuf {
    home.join(OMP_DIRECTORY).join(UNSETTLED)
}

/// The refusal while a marker exists, with its record when that is a bounded
/// regular file.
fn recorded(marker: &Path) -> Fault {
    let record = fs::symlink_metadata(marker)
        .ok()
        .filter(|metadata| metadata.is_file() && metadata.len() <= MAX_MARKER_BYTES)
        .and_then(|_| fs::read(marker).ok())
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
    Fault::new(
        "PluginUnsettled",
        "An OMP plugin command that MadoMata started for this user installation could not be confirmed to have ended. Confirm that this command and the processes it started are no longer running and review the installation, then delete the marker file to manage the plugin again",
    )
    .with_context(json!({"marker": display(marker), "record": record}))
}

/// The durable refusal shared by every application instance and data root of
/// this installation while the marker exists. It is checked only under
/// ownership, so a marker is never another owner's pending fence. A marker
/// that cannot be checked refuses as well.
pub(super) fn unsettled(home: &Path) -> Option<Fault> {
    let marker = unsettled_marker(home);
    match fs::symlink_metadata(&marker) {
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            None
        }
        Err(error) => Some(
            Fault::new(
                "PluginTargetUnverified",
                "The OMP user installation cannot be checked for unsettled plugin operations",
            )
            .with_context(json!({
                "marker": display(&marker),
                "kind": format!("{:?}", error.kind()),
                "os_code": error.raw_os_error(),
            })),
        ),
        Ok(_) => Some(recorded(&marker)),
    }
}

/// Creates the marker with `record`; `false` when a marker already exists.
fn create_marker(home: &Path, marker: &Path, record: &Value) -> io::Result<bool> {
    if let Err(error) = fs::create_dir(home.join(OMP_DIRECTORY))
        && error.kind() != io::ErrorKind::AlreadyExists
    {
        return Err(error);
    }
    let mut file = match OpenOptions::new().write(true).create_new(true).open(marker) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    file.write_all(&serde_json::to_vec(record)?)?;
    file.sync_all()?;
    Ok(true)
}

/// Reserves the marker for a mutation about to be dispatched under ownership,
/// so that a command which cannot be settled stays refused durably whatever
/// happens to this process.
///
/// # Errors
///
/// `PluginUnsettled` when a marker already exists, or
/// `PluginInstallationLock` when it cannot be recorded; nothing is dispatched.
pub(super) fn fence(home: &Path, record: &Value) -> Result<(), Fault> {
    let marker = unsettled_marker(home);
    match create_marker(home, &marker, record) {
        Ok(true) => Ok(()),
        Ok(false) => Err(recorded(&marker)),
        Err(error) => {
            // Nothing was dispatched, so a partial reservation is withdrawn.
            let _ = fs::remove_file(&marker);
            Err(Fault::new(
                "PluginInstallationLock",
                "Ownership of the OMP user installation could not be recorded; nothing was changed",
            )
            .with_context(json!({
                "marker": display(&marker),
                "kind": format!("{:?}", error.kind()),
                "os_code": error.raw_os_error(),
            })))
        }
    }
}

/// Lifts this mutation's fence once its command group is known to be gone. A
/// marker that cannot be removed keeps refusing.
pub(super) fn lift(home: &Path) {
    let _ = fs::remove_file(unsettled_marker(home));
}

/// Records that an owned command tree could not be settled; an existing
/// marker, such as a mutation's fence, is kept as it is.
///
/// # Errors
///
/// `PluginUnsettled` for this application instance alone when the record
/// cannot be written.
pub(super) fn mark_unsettled(home: &Path, record: &Value) -> Result<(), Fault> {
    let marker = unsettled_marker(home);
    create_marker(home, &marker, record)
        .map(|_| ())
        .map_err(|error| {
            Fault::new(
                "PluginUnsettled",
                "An OMP command started by this application could not be confirmed to have ended, and its marker could not be recorded; this application refuses plugin management",
            )
            .with_context(json!({
                "marker": display(&marker),
                "kind": format!("{:?}", error.kind()),
                "os_code": error.raw_os_error(),
            }))
        })
}

/// The host facts captured once at startup.
pub(super) struct Environment {
    pub(super) home: Result<PathBuf, Fault>,
    pub(super) search: Vec<PathBuf>,
    pub(super) path: Option<OsString>,
    pub(super) tmpdir: Option<OsString>,
    pub(super) variables: Vec<(&'static str, OsString)>,
}

impl Environment {
    pub(super) fn capture(home: Result<PathBuf, Fault>) -> Self {
        let path = std::env::var_os("PATH");
        let mut search: Vec<PathBuf> = path
            .as_deref()
            .map(|path| {
                std::env::split_paths(path)
                    .filter(|directory| directory.is_absolute())
                    .take(MAX_SEARCH)
                    .collect()
            })
            .unwrap_or_default();
        // GUI launches often lack the shell PATH; these are the usual user installations.
        search.extend(["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from));
        if let Ok(home) = &home {
            search.extend([".bun/bin", ".local/bin"].map(|directory| home.join(directory)));
        }
        let mut seen = BTreeSet::new();
        search.retain(|directory| seen.insert(directory.clone()));
        Self {
            home,
            search,
            path,
            tmpdir: std::env::var_os("TMPDIR"),
            variables: OVERRIDES
                .iter()
                .filter_map(|name| std::env::var_os(name).map(|value| (*name, value)))
                .collect(),
        }
    }

    /// Names of host variables that would make OMP sessions use state other than
    /// the normal user installation. They are refused, never silently cleared.
    pub(super) fn overrides(&self, home: &Path) -> Vec<&'static str> {
        self.variables
            .iter()
            .filter(|(name, value)| redirects(name, value, home, None))
            .map(|(name, _)| *name)
            .collect()
    }

    /// The complete child environment; nothing else is inherited.
    pub(super) fn child(&self, home: &Path, executable: &Path) -> Vec<(&'static str, OsString)> {
        let inherited = self
            .path
            .clone()
            .unwrap_or_else(|| OsString::from(DEFAULT_PATH));
        // OMP uninstall resolves Bun by PATH, including on minimal-PATH GUI launches.
        let path = executable
            .parent()
            .and_then(|directory| {
                std::env::join_paths(
                    std::iter::once(directory.to_path_buf())
                        .chain(self.search.iter().cloned())
                        .chain(std::env::split_paths(&inherited)),
                )
                .ok()
            })
            .unwrap_or(inherited);
        let mut environment = vec![
            ("HOME", home.as_os_str().to_owned()),
            ("PATH", path),
            ("TERM", OsString::from("dumb")),
            ("NO_COLOR", OsString::from("1")),
        ];
        if let Some(tmpdir) = &self.tmpdir {
            environment.push(("TMPDIR", tmpdir.clone()));
        }
        environment
    }
}

/// A resolved executable and the file identity it had when observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Executable {
    pub(super) path: PathBuf,
    fingerprint: Fingerprint,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Fingerprint {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl Fingerprint {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        }
    }
}

#[cfg(unix)]
fn runnable(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn runnable(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
}

fn executable(path: &Path) -> io::Result<Option<Executable>> {
    match fs::metadata(path) {
        Ok(metadata) if runnable(&metadata) => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    let canonical = path.canonicalize()?;
    let metadata = fs::metadata(&canonical)?;
    Ok(runnable(&metadata).then(|| Executable {
        fingerprint: Fingerprint::of(&metadata),
        path: canonical,
    }))
}

/// Finds exactly one `omp` in the search directories; several distinct files
/// are ambiguous and never resolved by order.
pub(super) fn discover(search: &[PathBuf]) -> Result<Executable, Refusal> {
    let mut found: Vec<Executable> = Vec::new();
    for directory in search {
        if let Ok(Some(candidate)) = executable(&directory.join(EXECUTABLE)) {
            if !found.iter().any(|known| known.path == candidate.path) {
                found.push(candidate);
            }
        }
    }
    match found.len() {
        0 => Err((
            PluginState::Missing,
            Fault::new(
                "PluginExecutableMissing",
                "No OMP executable was found; install OMP or select its executable",
            ),
        )),
        1 => Ok(found.remove(0)),
        _ => Err((
            PluginState::Unsupported,
            Fault::new(
                "PluginTargetAmbiguous",
                "Several OMP executables were found; select one explicitly",
            )
            .with_context(json!({
                "candidates": found.iter().map(|candidate| display(&candidate.path)).collect::<Vec<_>>()
            })),
        )),
    }
}

/// Checks an operator-selected executable path.
pub(super) fn explicit(text: &str) -> Result<Executable, Refusal> {
    let path = Path::new(text);
    if text.len() > MAX_PATH_BYTES || text.chars().any(char::is_control) || !path.is_absolute() {
        return Err((
            PluginState::Missing,
            Fault::new(
                "PluginExecutableInvalid",
                "Select the OMP executable by its absolute path",
            ),
        ));
    }
    selected(path)
}

/// Checks a selected or previously observed executable path.
pub(super) fn selected(path: &Path) -> Result<Executable, Refusal> {
    match executable(path) {
        Ok(Some(found)) => Ok(found),
        Ok(None) => Err((
            PluginState::Missing,
            Fault::new(
                "PluginExecutableMissing",
                "The selected path is not an executable file",
            )
            .with_context(json!({"executable": display(path)})),
        )),
        Err(error) => Err((
            PluginState::Missing,
            Fault::new(
                "PluginExecutableMissing",
                "The selected executable cannot be inspected",
            )
            .with_context(json!({
                "executable": display(path),
                "kind": format!("{:?}", error.kind()),
                "os_code": error.raw_os_error(),
            })),
        )),
    }
}

/// `omp --version` prints exactly `omp/<semver>`.
pub(super) fn omp_version(stdout: &[u8]) -> Option<Version> {
    let text = std::str::from_utf8(stdout).ok()?.trim();
    if text.contains('\n') {
        return None;
    }
    Version::parse(text.strip_prefix("omp/")?)
}

/// `omp plugin --help` lists the link, list and uninstall commands and JSON output.
pub(super) fn capable(help: &[u8]) -> bool {
    let text = String::from_utf8_lossy(help);
    ["link", "list", "uninstall", "--json"]
        .iter()
        .all(|required| {
            text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '-')
                .any(|word| word == *required)
        })
}

/// The known adapter's inventory facts; unrelated plugins are not retained.
pub(super) struct Inventory {
    pub(super) entry: Option<Entry>,
    /// A duplicate npm entry or a marketplace plugin with the adapter's name.
    pub(super) collision: bool,
}

pub(super) struct Entry {
    pub(super) version: Option<String>,
    pub(super) path: Option<String>,
}

pub(super) fn inventory(stdout: &[u8]) -> Result<Inventory, Fault> {
    let malformed = || {
        Fault::new(
            "PluginInventoryUnavailable",
            "OMP returned no recognizable plugin inventory",
        )
    };
    let value: Value = serde_json::from_slice(stdout).map_err(|_| malformed())?;
    let npm = value
        .get("npm")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?;
    let marketplace = value
        .get("marketplace")
        .and_then(Value::as_array)
        .ok_or_else(malformed)?;
    if npm.len().saturating_add(marketplace.len()) > MAX_INVENTORY {
        return Err(Fault::new(
            "PluginInventoryUnavailable",
            "OMP plugin inventory exceeds its entry bound",
        ));
    }
    let text =
        |plugin: &Value, key: &str| plugin.get(key).and_then(Value::as_str).map(str::to_owned);
    let mut entries = npm
        .iter()
        .filter(|plugin| plugin.get("name").and_then(Value::as_str) == Some(NAME));
    let entry = entries.next().map(|plugin| Entry {
        version: text(plugin, "version"),
        path: text(plugin, "path"),
    });
    let duplicate = entries.next().is_some();
    // A bare uninstall name resolves against marketplace plugins before npm.
    let marketplace = marketplace.iter().any(|plugin| {
        ["id", "name"]
            .iter()
            .filter_map(|key| plugin.get(*key).and_then(Value::as_str))
            .any(|id| id == NAME || id.rsplit_once('@').is_some_and(|(name, _)| name == NAME))
    });
    Ok(Inventory {
        entry,
        collision: duplicate || marketplace,
    })
}
