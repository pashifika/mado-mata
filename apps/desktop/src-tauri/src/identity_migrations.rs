//! Identifier-only conversion at lifecycle and explicit ingress boundaries.
use crate::configuration::{self, Capture, Kind, path_kind};
use crate::storage::{self, Profile, TabRecord, decode, encode, validate_id};
use crate::target::TargetRecord;
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

pub(crate) const LEDGER: &str = "identity-migrations.config";
pub(crate) const MAX_LEDGER_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 4096;
const ATTEMPTS: usize = 16;
const VALIDATION_ID: &str = "00000000000000000000";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum EntityKind {
    Profile,
    TargetBinding,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Assignment {
    kind: EntityKind,
    internal_name: String,
    package_id: String,
    legacy_id: String,
    xid: String,
}

impl Assignment {
    fn key(&self) -> (EntityKind, &str, &str, &str) {
        (
            self.kind,
            &self.internal_name,
            &self.package_id,
            &self.legacy_id,
        )
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ledger {
    version: u32,
    #[serde(deserialize_with = "bounded_assignments")]
    entries: Vec<Assignment>,
}

fn bounded_assignments<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<Assignment>, D::Error> {
    struct Object(Assignment);
    impl<'de> Deserialize<'de> for Object {
        fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            struct Visitor;
            impl<'de> serde::de::Visitor<'de> for Visitor {
                type Value = Object;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("an identity assignment object")
                }
                fn visit_map<A: serde::de::MapAccess<'de>>(
                    self,
                    map: A,
                ) -> Result<Object, A::Error> {
                    Assignment::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                        .map(Object)
                }
            }
            d.deserialize_map(Visitor)
        }
    }
    struct Entries;
    impl<'de> serde::de::Visitor<'de> for Entries {
        type Value = Vec<Assignment>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("at most 4096 identity assignments")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            let mut entries = Vec::new();
            while let Some(Object(entry)) = seq.next_element()? {
                if entries.len() == MAX_ENTRIES {
                    return Err(serde::de::Error::custom(
                        "identity mapping count exceeds 4096",
                    ));
                }
                entries.push(entry);
            }
            Ok(entries)
        }
    }
    d.deserialize_seq(Entries)
}

fn fault(message: &str) -> Fault {
    Fault::new("IdentityMigration", message)
}

/// This exact decoder is never used by ordinary storage getters or writers.
fn legacy(id: &str) -> bool {
    id.len() == 60
        && id.starts_with("p-")
        && id.as_bytes()[34] == b'-'
        && id.as_bytes()[43] == b'-'
        && id.as_bytes()[2..]
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 32 | 41) || b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

pub(crate) fn validate_ingress_id(id: &str) -> Result<(), Fault> {
    if legacy(id) { Ok(()) } else { validate_id(id) }
}

pub(crate) fn validate_profile(profile: &mut Profile, allow_legacy: bool) -> Result<(), Fault> {
    if allow_legacy && legacy(&profile.id) {
        let original = std::mem::replace(&mut profile.id, VALIDATION_ID.into());
        let result = storage::validate_profile(profile);
        profile.id = original;
        result
    } else {
        storage::validate_profile(profile)
    }
}

pub(crate) fn validate_target(
    record: &mut TargetRecord,
    tab: &str,
    package: &str,
    allow_legacy: bool,
) -> Result<(), Fault> {
    let original = record.binding.as_mut().and_then(|binding| {
        (allow_legacy && legacy(&binding.id))
            .then(|| std::mem::replace(&mut binding.id, VALIDATION_ID.into()))
    });
    let result = record.validate_owned(tab, package);
    if let Some(original) = original {
        record.binding.as_mut().expect("retained binding").id = original;
    }
    result
}

impl Ledger {
    fn empty() -> Self {
        Self {
            version: 1,
            entries: Vec::new(),
        }
    }

    fn read(files: &BTreeMap<String, Vec<u8>>) -> Result<Self, Fault> {
        files
            .get(LEDGER)
            .map_or_else(|| Ok(Self::empty()), |bytes| Self::decode(bytes))
    }

    fn decode(bytes: &[u8]) -> Result<Self, Fault> {
        if bytes.len() > MAX_LEDGER_BYTES {
            return Err(fault("identity mapping bytes exceed 4 MiB"));
        }
        let ledger: Self = decode(bytes)?;
        ledger.check()?;
        Ok(ledger)
    }

    fn check(&self) -> Result<(), Fault> {
        if self.version != 1 || self.entries.len() > MAX_ENTRIES {
            return Err(fault("unsupported identity mapping version or entry count"));
        }
        let mut keys = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        let mut aliases = BTreeMap::new();
        for entry in &self.entries {
            storage::validate_internal_name(&entry.internal_name)?;
            storage::validate_package_id(&entry.package_id)?;
            configuration::check_aliases(
                &format!(
                    "tabs/{}/{}/{}.config",
                    entry.internal_name, entry.package_id, entry.xid
                ),
                &mut aliases,
            )?;
            if !legacy(&entry.legacy_id)
                || validate_id(&entry.xid).is_err()
                || !keys.insert(entry.key())
                || !destinations.insert(&entry.xid)
            {
                return Err(fault(
                    "invalid, duplicate or conflicting identity assignment",
                ));
            }
        }
        Ok(())
    }

    fn merge(&mut self, other: Self) -> Result<(), Fault> {
        for entry in other.entries {
            if let Some(previous) = self.entries.iter().find(|old| old.key() == entry.key()) {
                if previous != &entry {
                    return Err(fault("identity mapping lineages conflict"));
                }
            } else {
                self.entries.push(entry);
            }
        }
        self.check()
    }

    fn assign(
        &mut self,
        kind: EntityKind,
        tab: &str,
        package: &str,
        id: &str,
        used: &mut BTreeSet<String>,
        generate: &mut impl FnMut() -> Result<String, Fault>,
    ) -> Result<String, Fault> {
        validate_ingress_id(id)?;
        if !legacy(id) {
            return Ok(id.to_owned());
        }
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| entry.key() == (kind, tab, package, id))
        {
            return Ok(entry.xid.clone());
        }
        if self.entries.len() == MAX_ENTRIES {
            return Err(fault("identity mapping count exceeds 4096"));
        }
        for _ in 0..ATTEMPTS {
            let xid = generate()?;
            validate_id(&xid)?;
            if used.contains(&xid) || self.entries.iter().any(|entry| entry.xid == xid) {
                continue;
            }
            used.insert(xid.clone());
            self.entries.push(Assignment {
                kind,
                internal_name: tab.into(),
                package_id: package.into(),
                legacy_id: id.into(),
                xid: xid.clone(),
            });
            return Ok(xid);
        }
        Err(fault(
            "could not allocate an unoccupied XID within 16 attempts",
        ))
    }

    fn publish(&mut self, files: &mut BTreeMap<String, Vec<u8>>) -> Result<(), Fault> {
        self.check()?;
        if self.entries.is_empty() && !files.contains_key(LEDGER) {
            return Ok(());
        }
        // Keep exact existing metadata bytes if its assignments did not change.
        if let Some(bytes) = files.get(LEDGER) {
            let old = Self::decode(bytes)?;
            if self.entries.len() == old.entries.len()
                && self.entries.iter().all(|entry| old.entries.contains(entry))
            {
                return Ok(());
            }
        }
        self.entries.sort();
        files.insert(LEDGER.into(), encode(self, MAX_LEDGER_BYTES)?);
        Ok(())
    }
}

pub(crate) fn validate_ledger(capture: &Capture) -> Result<(), Fault> {
    Ledger::read(&capture.files).map(|_| ())
}

pub(crate) fn reserved(root: &Path, id: &str) -> Result<bool, Fault> {
    Ok(root_ledger(root)?
        .entries
        .iter()
        .any(|entry| entry.xid == id))
}

fn root_ledger(root: &Path) -> Result<Ledger, Fault> {
    if !storage::exists(root)? {
        return Ok(Ledger::empty());
    }
    storage::check_directory(root)?;
    for (index, entry) in fs::read_dir(root)
        .map_err(|error| configuration::io_fault("inspect identity mapping root", error))?
        .enumerate()
    {
        if index >= configuration::MAX_ENUMERATED {
            return Err(fault("identity mapping root enumeration exceeds its bound"));
        }
        let entry =
            entry.map_err(|error| configuration::io_fault("read identity mapping root", error))?;
        if let Some(name) = entry.file_name().to_str() {
            let key = storage::filesystem_key(name);
            if key == "identity-migrations.pending" {
                return Err(fault("identity mapping has an unresolved pending write"));
            }
            if key == LEDGER && name != LEDGER {
                return Err(fault("identity mapping has a filesystem alias"));
            }
        }
    }
    let path = root.join(LEDGER);
    if !storage::exists(&path)? {
        return Ok(Ledger::empty());
    }
    Ledger::decode(&storage::read_bytes(&path, MAX_LEDGER_BYTES)?)
}

fn used_ids(capture: &Capture, ledger: &Ledger) -> Result<BTreeSet<String>, Fault> {
    let mut used = BTreeSet::new();
    for (path, bytes) in &capture.files {
        if path_kind(path)? != Kind::Package {
            continue;
        }
        let parts: Vec<_> = path.split('/').collect();
        let (kind, id) = if parts[3] == "target.config" {
            let target: TargetRecord = decode(bytes)?;
            let Some(binding) = target.binding else {
                continue;
            };
            (EntityKind::TargetBinding, binding.id)
        } else {
            let profile: Profile = decode(bytes)?;
            (EntityKind::Profile, profile.id)
        };
        if !legacy(&id) {
            if ledger.entries.iter().any(|entry| {
                entry.xid == id
                    && (entry.kind != kind
                        || entry.internal_name != parts[1]
                        || entry.package_id != parts[2])
            }) {
                return Err(fault(
                    "reserved XID is occupied by another owner or entity kind",
                ));
            }
            used.insert(id);
        }
    }
    Ok(used)
}

fn normalize_with(
    mut source: Capture,
    live: &Capture,
    generate: &mut impl FnMut() -> Result<String, Fault>,
) -> Result<Capture, Fault> {
    crate::restore::validate_owned(&source, true)?;
    let mut ledger = Ledger::read(&source.files)?;
    ledger.merge(Ledger::read(&live.files)?)?;
    let mut used = used_ids(&source, &ledger)?;
    let paths: Vec<_> = source.files.keys().cloned().collect();
    for path in paths {
        if path_kind(&path)? != Kind::Package {
            continue;
        }
        let parts: Vec<_> = path.split('/').collect();
        let bytes = &source.files[&path];
        if parts[3] == "target.config" {
            let mut record: TargetRecord = decode(bytes)?;
            if let Some(binding) = &mut record.binding {
                if legacy(&binding.id) {
                    binding.id = ledger.assign(
                        EntityKind::TargetBinding,
                        parts[1],
                        parts[2],
                        &binding.id,
                        &mut used,
                        generate,
                    )?;
                    source
                        .files
                        .insert(path, encode(&record, crate::target::MAX_TARGET_BYTES)?);
                }
            }
        } else {
            let mut profile: Profile = decode(bytes)?;
            if legacy(&profile.id) {
                profile.id = ledger.assign(
                    EntityKind::Profile,
                    parts[1],
                    parts[2],
                    &profile.id,
                    &mut used,
                    generate,
                )?;
                let destination = format!("tabs/{}/{}/{}.config", parts[1], parts[2], profile.id);
                if source.files.contains_key(&destination) {
                    return Err(fault("mapped profile destination already exists"));
                }
                source.files.remove(&path);
                source
                    .files
                    .insert(destination, encode(&profile, storage::MAX_PROFILE_BYTES)?);
            }
        }
    }
    ledger.publish(&mut source.files)?;
    let result = Capture::from_files(source.files, source.root_present)?;
    crate::restore::validate_owned(&result, false)?;
    Ok(result)
}

pub(crate) fn normalize_restore(source: Capture, live: &Capture) -> Result<Capture, Fault> {
    crate::restore::validate(&source)?;
    normalize_with(source, live, &mut storage::new_id)
}

/// Use one of the journal's exact ledger preimages even when an archive used
/// different JSON formatting. Interrupted rollback must never invent a third
/// byte generation that restart would correctly refuse as an external edit.
fn rollback_ledger<'a>(
    before: &'a Capture,
    after: &'a Capture,
) -> Result<Option<&'a Vec<u8>>, Fault> {
    let Some(next_bytes) = after.files.get(LEDGER) else {
        return Ok(before.files.get(LEDGER));
    };
    let previous = Ledger::read(&before.files)?;
    let next = Ledger::decode(next_bytes)?;
    if previous
        .entries
        .iter()
        .any(|entry| !next.entries.contains(entry))
    {
        return Err(fault(
            "rollback cannot discard or replace durable identity reservations",
        ));
    }
    if previous.entries.len() == next.entries.len() && before.files.contains_key(LEDGER) {
        Ok(before.files.get(LEDGER))
    } else {
        Ok(Some(next_bytes))
    }
}

pub(crate) fn check_rollback_budget(before: &Capture, after: &Capture) -> Result<(), Fault> {
    let retained = rollback_ledger(before, after)?;
    let old_size = before.files.get(LEDGER).map_or(0, Vec::len);
    let files =
        before.files.len() + usize::from(retained.is_some() && !before.files.contains_key(LEDGER));
    let bytes = before.files.values().map(Vec::len).sum::<usize>() - old_size
        + retained.map_or(0, Vec::len);
    if files > configuration::MAX_FILES || bytes > configuration::MAX_BYTES {
        return Err(fault(
            "retaining identity reservations on rollback exceeds the managed configuration budget",
        ));
    }
    Ok(())
}

/// Rollback restores user content, not the ability to allocate a second identity.
pub(crate) fn rollback_capture(before: &Capture, after: &Capture) -> Result<Capture, Fault> {
    let mut files = before.files.clone();
    if let Some(bytes) = rollback_ledger(before, after)? {
        files.insert(LEDGER.into(), bytes.clone());
    }
    Capture::from_files(files, before.root_present)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    Restore,
    IdentityMigration,
    ProfileImport {
        internal_name: String,
        package_id: String,
        source_id: String,
    },
}

/// Only checked automatic-migration and explicit-import planners construct this token.
pub(crate) struct Plan {
    before: Capture,
    after: Capture,
    operation: Operation,
}

impl Plan {
    pub(crate) fn into_parts(self) -> (Capture, Capture, Operation) {
        (self.before, self.after, self.operation)
    }
}

pub(crate) fn validate_transition(
    before: &Capture,
    after: &Capture,
    operation: &Operation,
) -> Result<(), Fault> {
    before.check()?;
    after.check()?;
    let previous = Ledger::read(&before.files)?;
    let assigned = Ledger::read(&after.files)?;
    for entry in &previous.entries {
        if !assigned.entries.contains(entry) {
            return Err(fault("transaction discards a durable identity reservation"));
        }
    }
    for entry in assigned
        .entries
        .iter()
        .filter(|entry| !previous.entries.contains(entry))
    {
        let eligible = match operation {
            Operation::Restore => true,
            Operation::ProfileImport {
                internal_name,
                package_id,
                source_id,
            } => {
                entry.key()
                    == (
                        EntityKind::Profile,
                        internal_name.as_str(),
                        package_id.as_str(),
                        source_id.as_str(),
                    )
            }
            Operation::IdentityMigration => {
                let prefix = format!("tabs/{}/{}", entry.internal_name, entry.package_id);
                match entry.kind {
                    EntityKind::Profile => before
                        .files
                        .contains_key(&format!("{prefix}/{}.config", entry.legacy_id)),
                    EntityKind::TargetBinding => before
                        .files
                        .get(&format!("{prefix}/target.config"))
                        .and_then(|bytes| decode::<TargetRecord>(bytes).ok())
                        .and_then(|record| record.binding)
                        .is_some_and(|binding| binding.id == entry.legacy_id),
                }
            }
        };
        if !eligible {
            return Err(fault(
                "journal reserves an identity outside its typed conversion scope",
            ));
        }
    }
    match operation {
        Operation::Restore => crate::restore::validate_installed(after),
        Operation::IdentityMigration => {
            let expected = normalize_with(before.clone(), after, &mut || {
                Err(fault("journal lacks a durable identity assignment"))
            })?;
            if expected != *after {
                return Err(fault("migration journal changes nonidentity content"));
            }
            Ok(())
        }
        Operation::ProfileImport {
            internal_name,
            package_id,
            source_id,
        } => {
            let path = format!("profiles/{source_id}.json");
            let bytes = before
                .files
                .get(&path)
                .ok_or_else(|| fault("import journal lacks its source preimage"))?;
            let profile: Profile = decode(bytes)?;
            if profile.id != *source_id || profile.package_id != *package_id {
                return Err(fault("import journal source owner differs"));
            }
            let (expected, _) =
                plan_import_with(before.clone(), internal_name, profile, after, &mut || {
                    Err(fault("import journal lacks a durable identity assignment"))
                })?;
            if expected != *after {
                return Err(fault("import journal changes unrelated configuration"));
            }
            Ok(())
        }
    }
}

fn plan_import_with(
    mut before: Capture,
    tab: &str,
    mut profile: Profile,
    reservations: &Capture,
    generate: &mut impl FnMut() -> Result<String, Fault>,
) -> Result<(Capture, String), Fault> {
    validate_profile(&mut profile, true)?;
    storage::validate_internal_name(tab)?;
    storage::validate_package_id(&profile.package_id)?;
    let owner: TabRecord = decode(
        before
            .files
            .get(&format!("tabs/{tab}/tab.config"))
            .ok_or_else(|| fault("import owner is missing"))?,
    )?;
    storage::validate_tab(&owner)?;
    if owner.internal_name != tab
        || !owner.open
        || !owner
            .packages
            .iter()
            .any(|p| p.package_id == profile.package_id)
    {
        return Err(fault("import owner is not an open bound Tab"));
    }
    let mut ledger = Ledger::read(&before.files)?;
    ledger.merge(Ledger::read(&reservations.files)?)?;
    // Other owners may have scoped malformed documents. Import never edits those bytes.
    let mut used: BTreeSet<String> = before
        .files
        .keys()
        .filter_map(|path| path.rsplit('/').next()?.strip_suffix(".config"))
        .filter(|id| validate_id(id).is_ok())
        .map(str::to_owned)
        .collect();
    for (path, bytes) in &before.files {
        if path.ends_with("/target.config") {
            if let Ok(record) = decode::<TargetRecord>(bytes) {
                if let Some(binding) = record.binding {
                    used.insert(binding.id);
                }
            }
        }
    }
    profile.id = ledger.assign(
        EntityKind::Profile,
        tab,
        &profile.package_id,
        &profile.id,
        &mut used,
        generate,
    )?;
    let destination = format!("tabs/{tab}/{}/{}.config", profile.package_id, profile.id);
    if let Some(bytes) = before.files.get(&destination) {
        let saved: Profile = decode(bytes)?;
        storage::validate_profile(&saved)?;
        if saved != profile {
            return Err(Fault::new(
                "LegacyConflict",
                "mapped profile differs from the imported content; neither file was changed",
            ));
        }
    } else {
        let prefix = format!("tabs/{tab}/{}/", profile.package_id);
        let mut count = 0;
        let mut total = 0;
        for (path, bytes) in &before.files {
            if path.starts_with(&prefix) && !path.ends_with("/target.config") {
                let saved: Profile = decode(bytes)?;
                storage::validate_profile(&saved)?;
                if saved.package_id != profile.package_id
                    || path != &format!("{prefix}{}.config", saved.id)
                {
                    return Err(fault(
                        "existing import owner profile differs from its filename",
                    ));
                }
                count += 1;
                total += bytes.len();
            }
        }
        let bytes = encode(&profile, storage::MAX_PROFILE_BYTES)?;
        if count >= storage::MAX_PROFILES || total + bytes.len() > storage::MAX_TOTAL_BYTES {
            return Err(fault("import exceeds this Tab/package profile budget"));
        }
        before.files.insert(destination, bytes);
    }
    ledger.publish(&mut before.files)?;
    Ok((Capture::from_files(before.files, true)?, profile.id))
}

pub(crate) fn import_profile(
    root: &Path,
    tab: &str,
    profile: Profile,
) -> Result<(String, bool), Fault> {
    let before = configuration::capture(root)?;
    let source_id = profile.id.clone();
    let package_id = profile.package_id.clone();
    let (after, id) =
        plan_import_with(before.clone(), tab, profile, &before, &mut storage::new_id)?;
    let unchanged = before
        .files
        .contains_key(&format!("tabs/{tab}/{package_id}/{id}.config"));
    let operation = Operation::ProfileImport {
        internal_name: tab.into(),
        package_id,
        source_id,
    };
    if before != after {
        crate::restore::install_migration(
            root,
            Plan {
                before,
                after,
                operation,
            },
        )
        .map_err(|mut error| {
            if error.context["configuration_installed"] == true {
                error.context["committed_profile_id"] = serde_json::json!(id);
                error.context["already_imported"] = serde_json::json!(unchanged);
            }
            error
        })?;
    } else {
        validate_transition(&before, &after, &operation)?;
    }
    Ok((id, unchanged))
}

// This probe deliberately does not promote unrelated scoped read failures to root
// failures. A found candidate is followed by strict full capture and typed validation.
fn has_candidate(directory: &Path, depth: usize, remaining: &mut usize) -> bool {
    let Ok(entries) = fs::read_dir(directory) else {
        return false;
    };
    for entry in entries.flatten() {
        if *remaining == 0 {
            return true;
        }
        *remaining -= 1;
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if depth < 2 && kind.is_dir() && has_candidate(&entry.path(), depth + 1, remaining) {
            return true;
        }
        if depth != 2 {
            continue;
        }
        if name.starts_with("p-") && name.ends_with(".config") {
            return true;
        }
        if name == "target.config" && kind.is_file() {
            if let Ok(bytes) = storage::read_bytes(&entry.path(), crate::target::MAX_TARGET_BYTES) {
                if let Ok(record) = decode::<TargetRecord>(&bytes) {
                    if record
                        .binding
                        .is_some_and(|binding| binding.id.starts_with("p-"))
                    {
                        return true;
                    }
                }
            }
        }
    }
    false
}

pub(crate) fn migrate(root: &Path) -> Result<(), Fault> {
    // Ledger corruption is root-scoped; absence never creates an empty ledger.
    root_ledger(root)?;
    let mut remaining = configuration::MAX_ENUMERATED;
    if !has_candidate(&root.join("tabs"), 0, &mut remaining) {
        return Ok(());
    }
    let before = configuration::capture(root)?;
    let after = normalize_with(before.clone(), &before, &mut storage::new_id)?;
    if after == before {
        return Ok(());
    }
    crate::restore::install_migration(
        root,
        Plan {
            before,
            after,
            operation: Operation::IdentityMigration,
        },
    )
}

#[cfg(test)]
pub(crate) mod tests;
