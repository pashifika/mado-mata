//! The normal per-user OMP target: environment, executable and inventory facts.

use super::PluginState;
use super::payload::NAME;
use super::version::Version;
use mado_runtime_comparison::model::Fault;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
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

pub(super) type Refusal = (PluginState, Fault);

pub(super) fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
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
            .filter(|(name, value)| {
                let text = value.to_string_lossy();
                let text = text.trim();
                !text.is_empty()
                    && match *name {
                        "OMP_PROFILE" | "PI_PROFILE" => text != "default",
                        "PI_CODING_AGENT_DIR" => {
                            Path::new(value) != home.join(OMP_DIRECTORY).join("agent")
                        }
                        "PI_CONFIG_DIR" => text != OMP_DIRECTORY,
                        // OMP prefers an existing `$XDG_DATA_HOME/omp` for plugin data.
                        _ => Path::new(value).join(EXECUTABLE).exists(),
                    }
            })
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
