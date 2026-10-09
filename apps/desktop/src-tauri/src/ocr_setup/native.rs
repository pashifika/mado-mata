//! Explicit, metadata-only native discovery. Never executes or loads a library.
mod binary;
#[cfg(test)]
mod tests;

use super::catalog::Platform;
use super::{check_cancel, fault, files};
use binary::{Budget, Image};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::AtomicBool;

const MAX_ROOTS: usize = 8;
const MAX_SUBDIRECTORIES: usize = 8;
const MAX_ENTRIES: usize = 32_768;
const MAX_CANDIDATES: usize = 8_192;
const MAX_LIBRARIES: usize = 64;
const MAX_CONTEXTS: usize = 256;
const MAX_EDGES: usize = 1_024;
const MAX_RPATHS: usize = 64;
const MAX_PATH: usize = 4_096;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum NativeSelection {
    Homebrew {},
    Folders { paths: Vec<String> },
}

/// Returns the complete bounded non-system metadata closure, not an ABI/init verdict.
pub(super) fn discover(
    platform: &Platform,
    selection: &NativeSelection,
    cancel: &AtomicBool,
) -> Result<Vec<String>, Fault> {
    check_cancel(cancel)?;
    let system_directory = if platform.os == "windows" {
        windows_system_directory()?
    } else {
        None
    };
    discover_with_system_directory(platform, selection, system_directory, cancel)
}

fn discover_with_system_directory(
    platform: &Platform,
    selection: &NativeSelection,
    system_directory: Option<PathBuf>,
    cancel: &AtomicBool,
) -> Result<Vec<String>, Fault> {
    check_cancel(cancel)?;
    if !matches!(
        (platform.os.as_str(), platform.arch.as_str()),
        ("macos", "aarch64") | ("windows", "x86_64")
    ) {
        return Err(fault(
            "native",
            "native discovery supports macOS arm64 and Windows x64 only",
        ));
    }
    let (paths, optional) = match selection {
        NativeSelection::Homebrew {} if platform.os == "macos" => {
            (platform.native.search_roots.as_slice(), true)
        }
        NativeSelection::Homebrew {} => {
            return Err(fault(
                "native",
                "Homebrew discovery is available only on macOS",
            ));
        }
        NativeSelection::Folders { paths } => (paths.as_slice(), false),
    };
    let mut scope = Scope::new(
        paths,
        &platform.native.search_subdirectories,
        optional,
        cancel,
    )?;
    if platform.os == "windows" {
        scope.system_directory = system_directory;
    }
    let candidates = scope.enumerate(&platform.os, cancel)?;
    let mut pending = VecDeque::new();
    for module in &platform.native.required_modules {
        check_cancel(cancel)?;
        let mut matches = BTreeSet::new();
        for (name, paths) in &candidates {
            check_cancel(cancel)?;
            if module_matches(name, module, &platform.os) {
                for path in paths {
                    check_cancel(cancel)?;
                    let resolved = scope.resolve(path)?.ok_or_else(|| missing(module, None))?;
                    matches.insert(resolved);
                }
            }
        }
        pending.push_back((unique(matches, module, None)?, Vec::<Runpath>::new()));
    }
    if pending.is_empty() {
        return Err(fault("native", "catalog has no required native modules"));
    }

    let mut budget = Budget::default();
    let mut images = BTreeMap::<PathBuf, Image>::new();
    let mut contexts = BTreeSet::new();
    let mut bindings = BTreeMap::new();
    let mut edges = 0;
    while let Some((path, inherited)) = pending.pop_front() {
        check_cancel(cancel)?;
        if !images.contains_key(&path) {
            if images.len() >= MAX_LIBRARIES {
                return Err(fault(
                    "native",
                    "dependency closure exceeds 64 libraries; choose a narrower installation",
                ));
            }
            images.insert(
                path.clone(),
                binary::inspect(&path, &platform.os, &mut budget, cancel)?,
            );
        }
        let image = &images[&path];
        let mut rpaths = Vec::new();
        for rpath in &image.rpaths {
            let entry = (path.clone(), rpath.clone());
            if !rpaths.contains(&entry) {
                rpaths.push(entry);
            }
        }
        for rpath in inherited {
            if !rpaths.contains(&rpath) {
                rpaths.push(rpath);
            }
        }
        if rpaths.len() > MAX_RPATHS {
            return Err(fault(
                "native",
                "dependency runpath stack exceeds 64 entries",
            ));
        }
        if !contexts.insert((path.clone(), rpaths.clone())) {
            continue;
        }
        if contexts.len() > MAX_CONTEXTS {
            return Err(fault(
                "native",
                "dependency traversal exceeds 256 loader contexts",
            ));
        }
        for name in &image.dependencies {
            check_cancel(cancel)?;
            edges += 1;
            if edges > MAX_EDGES {
                return Err(fault(
                    "native",
                    "dependency traversal exceeds 1024 references",
                ));
            }
            let dependency = if platform.os == "macos" {
                resolve_mach(&scope, &path, name, &rpaths, cancel)?
            } else {
                resolve_pe(&scope, &candidates, &path, name, cancel)?
            };
            let key = (path.clone(), name.clone());
            if let Some(previous) = bindings.insert(key, dependency.clone())
                && previous != dependency
            {
                return Err(fault(
                    "native",
                    &format!(
                        "ambiguous dependency {name} from {}; loader contexts select different libraries",
                        path.display()
                    ),
                ));
            }
            if let Some(dependency) = dependency {
                pending.push_back((dependency, rpaths.clone()));
            }
        }
    }
    // Recheck containment and file stability before the shared native header/name gate.
    let mut closure = Vec::with_capacity(images.len());
    for (path, image) in images {
        check_cancel(cancel)?;
        if scope.resolve(&path)?.as_ref() != Some(&path) {
            return Err(fault(
                "native",
                "native library location changed during discovery",
            ));
        }
        image.unchanged(&path)?;
        closure.push(path_text(&path)?.to_owned());
    }
    files::native(&closure, platform, cancel)
}

// Keep the declaring image: inherited @loader_path entries belong to that image.
type Runpath = (PathBuf, String);

type Candidates = BTreeMap<String, Vec<PathBuf>>;

struct Scope {
    roots: Vec<PathBuf>,
    lexical_roots: Vec<PathBuf>,
    directories: Vec<PathBuf>,
    // Exact runtime basenames only; this directory is never enumerated.
    system_directory: Option<PathBuf>,
}

impl Scope {
    fn new(
        paths: &[String],
        subdirectories: &[String],
        optional: bool,
        cancel: &AtomicBool,
    ) -> Result<Self, Fault> {
        if !(1..=MAX_ROOTS).contains(&paths.len()) {
            return Err(fault(
                "native",
                "approve between 1 and 8 installation folders",
            ));
        }
        if subdirectories.len() > MAX_SUBDIRECTORIES {
            return Err(fault(
                "native",
                "catalog exceeds 8 native search subdirectories",
            ));
        }
        for subdirectory in subdirectories {
            super::catalog::safe_relative(subdirectory)?;
            if subdirectory.len() > MAX_PATH {
                return Err(fault("native", "native search subdirectory is too long"));
            }
        }
        let mut roots = BTreeSet::new();
        let mut lexical_roots = Vec::new();
        for text in paths {
            check_cancel(cancel)?;
            let path = normalized(Path::new(text))?;
            let canonical = match fs::canonicalize(text) {
                Ok(path) => path,
                Err(error) if optional && error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(path_fault(&path, error)),
            };
            path_text(&canonical)?;
            if !fs::metadata(&canonical)
                .map_err(|error| path_fault(&path, error))?
                .is_dir()
            {
                return Err(fault(
                    "native",
                    &format!("approved folder is not a directory: {}", path.display()),
                ));
            }
            // A fixed Homebrew prefix cannot grant authority to a redirected tree.
            if optional && canonical != path {
                return Err(outside(&canonical));
            }
            lexical_roots.push(path);
            roots.insert(canonical);
        }
        if roots.is_empty() {
            return Err(fault(
                "missing",
                "no approved Homebrew installation folder exists; install OpenCV or choose its installation folder",
            ));
        }
        let mut scope = Self {
            roots: roots.into_iter().collect(),
            lexical_roots,
            directories: Vec::new(),
            system_directory: None,
        };
        let mut directories = BTreeSet::new();
        for root in &scope.roots {
            directories.insert(root.clone());
            for subdirectory in subdirectories {
                check_cancel(cancel)?;
                let path = root.join(subdirectory);
                if let Some(canonical) = scope.resolve(&path)? {
                    if !fs::metadata(&canonical)
                        .map_err(|error| path_fault(&path, error))?
                        .is_dir()
                    {
                        return Err(fault(
                            "native",
                            &format!("candidate folder is not a directory: {}", path.display()),
                        ));
                    }
                    directories.insert(canonical);
                }
            }
        }
        scope.directories = directories.into_iter().collect();
        Ok(scope)
    }

    fn contains(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
            || self.system_directory.as_deref().is_some_and(|directory| {
                path.parent() == Some(directory)
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(windows_msvc_runtime)
            })
    }

    fn resolve(&self, path: &Path) -> Result<Option<PathBuf>, Fault> {
        let lexical = normalized(path)?;
        if !self.contains(&lexical)
            && !self
                .lexical_roots
                .iter()
                .any(|root| lexical.starts_with(root))
        {
            return Err(outside(path));
        }
        match fs::canonicalize(path) {
            Ok(canonical) => {
                path_text(&canonical)?;
                if !self.contains(&canonical) {
                    return Err(outside(&canonical));
                }
                Ok(Some(canonical))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(path_fault(&path, error)),
        }
    }

    fn enumerate(&self, os: &str, cancel: &AtomicBool) -> Result<Candidates, Fault> {
        let mut candidates = Candidates::new();
        let mut libraries = 0;
        let mut entries = 0;
        for directory in &self.directories {
            check_cancel(cancel)?;
            if self.resolve(directory)?.as_ref() != Some(directory) {
                return Err(fault("native", "candidate folder changed during discovery"));
            }
            for entry in fs::read_dir(directory).map_err(|error| path_fault(directory, error))? {
                check_cancel(cancel)?;
                entries += 1;
                if entries > MAX_ENTRIES {
                    return Err(fault(
                        "native",
                        "discovery exceeds 32768 directory entries; select a narrower installation folder",
                    ));
                }
                let entry = entry.map_err(|error| path_fault(directory, error))?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let library = if os == "macos" {
                    name.ends_with(".dylib")
                } else {
                    name.rsplit_once('.')
                        .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case("dll"))
                };
                if library {
                    libraries += 1;
                    if libraries > MAX_CANDIDATES {
                        return Err(fault(
                            "native",
                            "discovery exceeds 8192 library candidates; select a narrower installation folder",
                        ));
                    }
                    let name = if os == "windows" {
                        name.to_ascii_lowercase()
                    } else {
                        name.to_owned()
                    };
                    candidates.entry(name).or_default().push(entry.path());
                }
            }
        }
        Ok(candidates)
    }
}

fn module_matches(name: &str, module: &str, os: &str) -> bool {
    if os == "windows" {
        name.eq_ignore_ascii_case(module)
    } else {
        name.strip_prefix(module)
            .is_some_and(|suffix| suffix.starts_with('.') && suffix.ends_with(".dylib"))
    }
}

fn resolve_mach(
    scope: &Scope,
    loader: &Path,
    name: &str,
    rpaths: &[Runpath],
    cancel: &AtomicBool,
) -> Result<Option<PathBuf>, Fault> {
    // Only explicit protected install names are OS-provided. An arbitrary
    // @rpath name does not become a system library merely through a search path.
    if mac_system(Path::new(name)) {
        return Ok(None);
    }
    if let Some(relative) = name.strip_prefix("@rpath/") {
        for (owner, rpath) in rpaths {
            check_cancel(cancel)?;
            let directory = expand_loader(rpath, owner)?;
            if let Some(path) = scope.resolve(&directory.join(relative))? {
                // dyld stops at the first existing entry in the declared runpath order.
                return Ok(Some(path));
            }
        }
    } else {
        check_cancel(cancel)?;
        if let Some(path) = scope.resolve(&expand_loader(name, loader)?)? {
            return Ok(Some(path));
        }
    }
    Err(missing(name, Some(loader)))
}

fn resolve_pe(
    scope: &Scope,
    candidates: &Candidates,
    loader: &Path,
    name: &str,
    cancel: &AtomicBool,
) -> Result<Option<PathBuf>, Fault> {
    if windows_system(name) {
        return Ok(None);
    }
    let mut matches = BTreeSet::new();
    // DLL basenames are searched only in the explicitly enumerated directories.
    if let Some(paths) = candidates.get(name) {
        for path in paths {
            check_cancel(cancel)?;
            matches.insert(
                scope
                    .resolve(path)?
                    .ok_or_else(|| missing(name, Some(loader)))?,
            );
        }
    }
    // Prefer an explicitly selected installation. Only missing, known MSVC
    // imports may use the OS-reported directory, never PATH or an arbitrary DLL.
    if matches.is_empty()
        && windows_msvc_runtime(name)
        && let Some(directory) = &scope.system_directory
        && let Some(path) = scope.resolve(&directory.join(name))?
    {
        matches.insert(path);
    }
    unique(matches, name, Some(loader)).map(Some)
}

fn unique(paths: BTreeSet<PathBuf>, name: &str, loader: Option<&Path>) -> Result<PathBuf, Fault> {
    if paths.len() > 1 {
        return Err(fault(
            "native",
            &format!(
                "ambiguous native dependency {name}: {}; choose one installation or remove the extra approved folder",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        ));
    }
    paths
        .into_iter()
        .next()
        .ok_or_else(|| missing(name, loader))
}

fn missing(name: &str, loader: Option<&Path>) -> Fault {
    let source = loader
        .map(|path| format!(" (required by {})", path.display()))
        .unwrap_or_default();
    fault(
        "missing",
        &format!(
            "missing native dependency {name}{source}; install it or add its installation folder and check the selection again"
        ),
    )
}

fn outside(path: &Path) -> Fault {
    fault(
        "native",
        &format!(
            "native dependency is outside approved folders: {}; add its installation folder and check the selection again",
            path.display()
        ),
    )
}

fn path_fault(path: &Path, error: std::io::Error) -> Fault {
    fault(
        if error.kind() == std::io::ErrorKind::NotFound {
            "missing"
        } else {
            "native"
        },
        &format!("{}: {error}", path.display()),
    )
}

fn path_text(path: &Path) -> Result<&str, Fault> {
    let text = path
        .to_str()
        .ok_or_else(|| fault("native", "native path is not UTF-8"))?;
    if text.len() > MAX_PATH || text.chars().any(char::is_control) {
        return Err(fault(
            "native",
            "native paths must be at most 4096 bytes without control characters",
        ));
    }
    Ok(text)
}

fn normalized(path: &Path) -> Result<PathBuf, Fault> {
    path_text(path)?;
    if !path.is_absolute() {
        return Err(fault(
            "native",
            "approve an absolute installation folder or dependency path",
        ));
    }
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                if !result.pop() {
                    return Err(fault(
                        "native",
                        "native path traverses above its filesystem root",
                    ));
                }
            }
            Component::CurDir => {}
            _ => result.push(component.as_os_str()),
        }
    }
    Ok(result)
}

fn expand_loader(value: &str, loader: &Path) -> Result<PathBuf, Fault> {
    if value == "@loader_path" {
        return Ok(loader.parent().unwrap().to_path_buf());
    }
    if let Some(relative) = value.strip_prefix("@loader_path/") {
        let path = loader.parent().unwrap().join(relative);
        normalized(&path)?;
        return Ok(path);
    }
    if Path::new(value).is_absolute() {
        normalized(Path::new(value))?;
        return Ok(PathBuf::from(value));
    }
    Err(fault(
        "native",
        &format!(
            "unsupported dependency/runpath {value} in {}; only absolute, @loader_path and declared @rpath dependencies can be resolved without loading",
            loader.display()
        ),
    ))
}

fn mac_system(path: &Path) -> bool {
    (path.starts_with("/usr/lib") || path.starts_with("/System/Library"))
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

#[cfg(windows)]
fn windows_system_directory() -> Result<Option<PathBuf>, Fault> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let mut buffer = [0_u16; MAX_PATH];
    #[expect(unsafe_code, reason = "bounded OS system-directory query")]
    // SAFETY: buffer is writable for the supplied number of UTF-16 units;
    // the API retains no pointer. The returned length is checked before use.
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 {
        return Err(fault(
            "native",
            &format!(
                "cannot locate Windows system directory: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    if length >= buffer.len() {
        return Err(fault(
            "native",
            "Windows system directory exceeds path limit",
        ));
    }
    let path = PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
    let canonical = fs::canonicalize(&path).map_err(|error| path_fault(&path, error))?;
    path_text(&canonical)?;
    Ok(Some(canonical))
}

#[cfg(not(windows))]
fn windows_system_directory() -> Result<Option<PathBuf>, Fault> {
    Ok(None)
}

fn windows_msvc_runtime(name: &str) -> bool {
    [
        "concrt140.dll",
        "msvcp140.dll",
        "msvcp140_1.dll",
        "msvcp140_2.dll",
        "msvcp140_atomic_wait.dll",
        "msvcp140_codecvt_ids.dll",
        "vcruntime140.dll",
        "vcruntime140_1.dll",
        "vcomp140.dll",
    ]
    .iter()
    .any(|known| name.eq_ignore_ascii_case(known))
}

fn windows_system(name: &str) -> bool {
    // PE metadata names have already been validated and ASCII-lowercased.
    // API sets and UCRT belong to supported modern Windows. Redistributable
    // vcruntime/msvcp/concrt/vcomp DLLs deliberately remain non-system imports.
    ((name.starts_with("api-ms-win-") || name.starts_with("ext-ms-win-")) && name.ends_with(".dll"))
        || matches!(
            name,
            "kernel32.dll"
                | "kernelbase.dll"
                | "ntdll.dll"
                | "user32.dll"
                | "gdi32.dll"
                | "gdi32full.dll"
                | "advapi32.dll"
                | "shell32.dll"
                | "ole32.dll"
                | "oleaut32.dll"
                | "combase.dll"
                | "comdlg32.dll"
                | "comctl32.dll"
                | "ws2_32.dll"
                | "shlwapi.dll"
                | "version.dll"
                | "winmm.dll"
                | "imm32.dll"
                | "setupapi.dll"
                | "rpcrt4.dll"
                | "secur32.dll"
                | "sspicli.dll"
                | "bcrypt.dll"
                | "bcryptprimitives.dll"
                | "crypt32.dll"
                | "cryptbase.dll"
                | "msvcrt.dll"
                | "ucrtbase.dll"
                | "normaliz.dll"
                | "powrprof.dll"
                | "iphlpapi.dll"
                | "d3d9.dll"
                | "d3d11.dll"
                | "d3d12.dll"
                | "dxgi.dll"
                | "dxva2.dll"
                | "opengl32.dll"
                | "glu32.dll"
                | "mf.dll"
                | "mfplat.dll"
                | "mfreadwrite.dll"
                | "propsys.dll"
                | "avrt.dll"
                | "dwmapi.dll"
                | "wintrust.dll"
                | "winspool.drv"
                | "msacm32.dll"
                | "msvfw32.dll"
                | "avifil32.dll"
                | "msimg32.dll"
                | "psapi.dll"
                | "dbghelp.dll"
                | "cfgmgr32.dll"
                | "shcore.dll"
                | "winhttp.dll"
                | "wininet.dll"
                | "dnsapi.dll"
                | "userenv.dll"
                | "wtsapi32.dll"
        )
}
