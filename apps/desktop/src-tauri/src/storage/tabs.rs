use super::fs::{bounded_entries, check_alias, check_directory, limit, storage};
use super::{
    MAX_OPEN_TABS, MAX_PACKAGES, MAX_PATH_BYTES, MAX_TAB_BYTES, MAX_TABS, Store, VERSION,
    check_budget, decode, encode, exists, filesystem_key, private_directory, read_bytes,
    validate_package_id, write_atomic,
};
use crate::configuration::publish_no_replace;
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TabRecord {
    pub version: u32,
    pub internal_name: String,
    pub display_name: String,
    pub open: bool,
    pub packages: Vec<PackageReference>,
    pub selected_package_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PackageReference {
    pub package_id: String,
    pub source: PackageSource,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackageSource {
    Directory { path: String },
    CustomArchive { path: String },
}

#[derive(Clone, Debug, Serialize)]
pub struct TabListing {
    pub tabs: Vec<TabRecord>,
    pub faults: Vec<Fault>,
}

impl Store {
    pub fn tabs(&self) -> Result<TabListing, Fault> {
        check_directory(&self.root)?;
        check_alias(&self.root, "tabs")?;
        let directory = self.root.join("tabs");
        let mut listing = TabListing {
            tabs: Vec::new(),
            faults: Vec::new(),
        };
        if !exists(&directory)? {
            return Ok(listing);
        }
        check_directory(&directory)?;
        let entries = bounded_entries(&directory)?;
        for entry in entries {
            let filename = entry.file_name();
            let name = filename.to_string_lossy();
            let result = (|| {
                let metadata = fs::symlink_metadata(entry.path())
                    .map_err(|error| storage("inspect Tab container", error))?;
                if metadata.is_file() {
                    return Ok(None);
                }
                validate_internal_name(&name)?;
                check_directory(&entry.path())?;
                if !exists(&entry.path().join("tab.config"))? {
                    if bounded_entries(&entry.path())?.is_empty() {
                        return Ok(None);
                    }
                    return Err(Fault::new(
                        "TabOrphan",
                        "nonempty Tab container has no tab.config; original data was preserved",
                    ));
                }
                self.tab(&name).map(Some)
            })();
            match result {
                Ok(Some(tab)) => listing.tabs.push(tab),
                Ok(None) => {}
                Err(fault) => listing.faults.push(tab_fault(fault, &name)),
            }
            if listing.tabs.len() + listing.faults.len() > MAX_TABS {
                return Err(limit("saved Tab count exceeds 64"));
            }
        }
        if listing.tabs.iter().filter(|tab| tab.open).count() > MAX_OPEN_TABS {
            return Err(limit("saved open Tab count exceeds eight"));
        }
        listing
            .tabs
            .sort_by(|a, b| a.internal_name.cmp(&b.internal_name));
        Ok(listing)
    }

    pub fn tab(&self, name: &str) -> Result<TabRecord, Fault> {
        let result = (|| {
            let directory = self.tab_directory(name)?;
            check_directory(&directory)?;
            check_alias(&directory, "tab.config")?;
            let tab: TabRecord =
                decode(&read_bytes(&directory.join("tab.config"), MAX_TAB_BYTES)?)?;
            validate_tab(&tab)?;
            if tab.internal_name != name {
                return Err(Fault::new(
                    "TabIdentity",
                    "Tab name does not match its containing directory",
                ));
            }
            for package in &tab.packages {
                check_alias(&directory, &package.package_id)?;
                let path = directory.join(&package.package_id);
                if exists(&path)? {
                    check_directory(&path)?;
                }
            }
            Ok(tab)
        })();
        result.map_err(|fault| tab_fault(fault, name))
    }

    pub fn create_tab(&self, internal_name: &str, display_name: &str) -> Result<TabRecord, Fault> {
        let tab = TabRecord {
            version: VERSION,
            internal_name: internal_name.to_owned(),
            display_name: if display_name.is_empty() {
                internal_name
            } else {
                display_name
            }
            .to_owned(),
            open: true,
            packages: Vec::new(),
            selected_package_id: None,
        };
        validate_tab(&tab)?;
        let listing = self.tabs()?;
        if listing.tabs.len() + listing.faults.len() >= MAX_TABS {
            return Err(limit("saved Tab count exceeds 64"));
        }
        ensure_open_slot(&listing)?;
        let tabs = self.root.join("tabs");
        private_directory(&tabs)?;
        check_alias(&tabs, internal_name)?;
        let directory = tabs.join(internal_name);
        if exists(&directory)? {
            check_directory(&directory)?;
            if !bounded_entries(&directory)?.is_empty() {
                return Err(tab_fault(
                    Fault::new(
                        "TabExists",
                        "this internal name is already saved or contains retained data",
                    ),
                    internal_name,
                ));
            }
        }
        let bytes = encode(&tab, MAX_TAB_BYTES)?;
        check_budget(
            &self.root,
            &format!("tabs/{internal_name}/tab.config"),
            bytes.len(),
        )?;
        private_directory(&directory)?;
        write_atomic(&directory.join("tab.config"), &bytes, publish_no_replace)?;
        Ok(tab)
    }

    pub fn set_tab_open(&self, name: &str, open: bool) -> Result<TabRecord, Fault> {
        let mut tab = self.tab(name)?;
        if tab.open == open {
            return Ok(tab);
        }
        if open {
            ensure_open_slot(&self.tabs()?)?;
        }
        tab.open = open;
        self.write_tab(&tab)?;
        Ok(tab)
    }

    pub fn bind_package(
        &self,
        name: &str,
        package_id: &str,
        path: &Path,
    ) -> Result<TabRecord, Fault> {
        validate_package_id(package_id)?;
        let mut tab = self.tab(name)?;
        if !tab.open {
            return Err(tab_fault(
                Fault::new("TabClosed", "reopen the Tab before binding a package"),
                name,
            ));
        }
        let path = path
            .to_str()
            .ok_or_else(|| Fault::new("PackageSource", "package source must be UTF-8"))?;
        validate_source_path(path)?;
        let directory = self.tab_directory(name)?;
        check_alias(&directory, package_id)?;
        if exists(&directory.join(package_id))? {
            check_directory(&directory.join(package_id))?;
        }
        let source = PackageSource::Directory {
            path: path.to_owned(),
        };
        if let Some(reference) = tab
            .packages
            .iter_mut()
            .find(|reference| reference.package_id == package_id)
        {
            reference.source = source;
        } else {
            tab.packages.push(PackageReference {
                package_id: package_id.to_owned(),
                source,
            });
        }
        tab.selected_package_id = Some(package_id.to_owned());
        self.write_tab(&tab)?;
        Ok(tab)
    }

    fn tab_directory(&self, name: &str) -> Result<PathBuf, Fault> {
        validate_internal_name(name)?;
        check_directory(&self.root)?;
        check_alias(&self.root, "tabs")?;
        let directory = self.root.join("tabs");
        check_directory(&directory)?;
        check_alias(&directory, name)?;
        Ok(directory.join(name))
    }

    fn write_tab(&self, tab: &TabRecord) -> Result<(), Fault> {
        validate_tab(tab)?;
        let directory = self.tab_directory(&tab.internal_name)?;
        let bytes = encode(tab, MAX_TAB_BYTES)?;
        check_budget(
            &self.root,
            &format!("tabs/{}/tab.config", tab.internal_name),
            bytes.len(),
        )?;
        write_atomic(&directory.join("tab.config"), &bytes, |from, to| {
            fs::rename(from, to)
        })
        .map_err(|fault| tab_fault(fault, &tab.internal_name))
    }
}

fn tab_fault(mut fault: Fault, name: &str) -> Fault {
    fault.context["internal_name"] = json!(name);
    fault
}
fn ensure_open_slot(listing: &TabListing) -> Result<(), Fault> {
    // An unreadable record may be open; reserve its slot rather than exceed the bound.
    if listing.tabs.iter().filter(|tab| tab.open).count() + listing.faults.len() >= MAX_OPEN_TABS {
        return Err(limit(
            "open Tab count exceeds eight, including unresolved saved records",
        ));
    }
    Ok(())
}
pub(crate) fn validate_internal_name(name: &str) -> Result<(), Fault> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(Fault::new(
            "TabName",
            "internal name must contain 1 to 64 ASCII letters, digits, underscores or hyphens",
        ));
    }
    Ok(())
}
fn validate_source_path(path: &str) -> Result<(), Fault> {
    if path.is_empty()
        || path.len() > MAX_PATH_BYTES
        || !Path::new(path).is_absolute()
        || path.chars().any(char::is_control)
    {
        return Err(Fault::new(
            "PackageSource",
            "package source must be an absolute UTF-8 path of at most 4096 bytes",
        ));
    }
    Ok(())
}

pub(crate) fn validate_tab(tab: &TabRecord) -> Result<(), Fault> {
    if tab.version != VERSION {
        return Err(Fault::new(
            "TabVersion",
            "unsupported Tab version; original data was preserved",
        ));
    }
    validate_internal_name(&tab.internal_name)?;
    if tab.display_name.trim().is_empty()
        || tab.display_name.chars().count() > 80
        || tab.display_name.chars().any(char::is_control)
    {
        return Err(Fault::new(
            "TabName",
            "display name must contain 1 to 80 Unicode scalars, be nonblank and contain no controls",
        ));
    }
    if tab.packages.len() > MAX_PACKAGES {
        return Err(limit("package references per Tab exceed 16"));
    }
    let mut identities = BTreeSet::new();
    for reference in &tab.packages {
        validate_package_id(&reference.package_id)?;
        if !identities.insert(filesystem_key(&reference.package_id)) {
            return Err(Fault::new(
                "StorageAlias",
                "Tab package references contain duplicate or aliased identities",
            ));
        }
        match &reference.source {
            PackageSource::Directory { path } | PackageSource::CustomArchive { path } => {
                validate_source_path(path)?
            }
        }
    }
    if tab.selected_package_id.as_ref().is_some_and(|id| {
        !tab.packages
            .iter()
            .any(|reference| &reference.package_id == id)
    }) {
        return Err(Fault::new(
            "TabIdentity",
            "selected package is not recorded in this Tab",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
