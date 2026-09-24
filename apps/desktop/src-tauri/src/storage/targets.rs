use super::fs::check_alias;
use super::{
    Store, check_budget, decode, encode, exists, new_id, private_directory, read_bytes,
    write_atomic,
};
use crate::target::{
    self, MAX_TARGET_BYTES, TargetBinding, TargetCheck, TargetConfiguration, TargetExpectation,
    TargetRecord, TargetResolution,
};
use mado_runtime_comparison::inventory::TargetDeclaration;
use mado_runtime_comparison::model::Fault;
use serde_json::json;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

impl Store {
    pub fn read_target(&self, tab: &str, package: &str) -> Result<TargetRecord, Fault> {
        let result = (|| {
            let directory = self.target_directory(tab, package)?;
            let mut record = TargetRecord {
                version: target::TARGET_VERSION,
                internal_name: tab.to_owned(),
                package_id: package.to_owned(),
                revision: 0,
                binding: None,
            };
            if !exists(&directory)? {
                return Ok(record);
            }
            check_alias(&directory, "target.config")?;
            check_alias(&directory, "target.pending")?;
            if exists(&directory.join("target.pending"))? {
                return Err(Fault::new(
                    "StoragePending",
                    "target configuration has an unresolved pending file; preserve or repair it first",
                ));
            }
            let path = directory.join("target.config");
            if exists(&path)? {
                record = decode(&read_bytes(&path, MAX_TARGET_BYTES)?)?;
                record.validate_owned(tab, package)?;
            }
            Ok(record)
        })();
        result.map_err(|fault| target_fault(fault, tab, package))
    }

    pub fn check_target(
        &self,
        tab: &str,
        package: &str,
        declaration: &TargetDeclaration,
        expected: &TargetExpectation,
        configuration: &TargetConfiguration,
    ) -> Result<TargetCheck, Fault> {
        let result = (|| {
            let record = self.read_target(tab, package)?;
            record.compare(expected)?;
            target::check(configuration, declaration, record.binding.as_ref())
        })();
        result.map_err(|fault| target_fault(fault, tab, package))
    }

    /// Caller holds the Store mutex and command guard. Revisions protect drafts
    /// in the supported single-instance root, not arbitrary external writers.
    pub fn save_target(
        &self,
        tab: &str,
        package: &str,
        declaration: &TargetDeclaration,
        expected: &TargetExpectation,
        configuration: TargetConfiguration,
        reviewed_resolution: Option<&TargetResolution>,
    ) -> Result<(TargetRecord, TargetCheck), Fault> {
        let result = (|| {
            let mut record = self.read_target(tab, package)?;
            record.compare(expected)?;
            let revision = record.next_revision()?;
            let check = target::check(&configuration, declaration, record.binding.as_ref())?;
            if (check.resolution_changed && reviewed_resolution != Some(&check.resolution))
                || reviewed_resolution.is_some_and(|reviewed| reviewed != &check.resolution)
            {
                return Err(Fault::new(
                    "TargetResolutionChanged",
                    "canonical target locations changed; review them before saving",
                )
                .with_context(json!({
                    "previous_resolution": check.previous_resolution,
                    "resolution": check.resolution,
                })));
            }
            let declaration_identity = declaration.identity()?;
            let id = match &record.binding {
                Some(binding)
                    if binding.compatible(package, &declaration.id, &declaration_identity) =>
                {
                    binding.id.clone()
                }
                _ => new_id()?,
            };
            record.revision = revision;
            record.binding = Some(TargetBinding {
                id,
                package_id: package.to_owned(),
                target_id: declaration.id.clone(),
                declaration_identity,
                configuration,
                resolution: check.resolution.clone(),
            });
            self.write_target(&record, |from, to| fs::rename(from, to))?;
            Ok((record, check))
        })();
        result.map_err(|fault| target_fault(fault, tab, package))
    }

    pub fn remove_target(
        &self,
        tab: &str,
        package: &str,
        expected: &TargetExpectation,
    ) -> Result<TargetRecord, Fault> {
        let result = (|| {
            let mut record = self.read_target(tab, package)?;
            record.compare(expected)?;
            record.revision = record.next_revision()?;
            record.binding = None;
            self.write_target(&record, |from, to| fs::rename(from, to))?;
            Ok(record)
        })();
        result.map_err(|fault| target_fault(fault, tab, package))
    }

    fn target_directory(&self, tab: &str, package: &str) -> Result<PathBuf, Fault> {
        // The same saved Tab/package ownership rule as profiles, without reading profiles.
        Ok(self.profile_store(tab, package)?.directory())
    }

    fn write_target(
        &self,
        record: &TargetRecord,
        replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
    ) -> Result<(), Fault> {
        record.validate_owned(&record.internal_name, &record.package_id)?;
        let directory = self.target_directory(&record.internal_name, &record.package_id)?;
        let bytes = encode(record, MAX_TARGET_BYTES)?;
        check_budget(
            &self.root,
            &format!(
                "tabs/{}/{}/target.config",
                record.internal_name, record.package_id
            ),
            bytes.len(),
        )?;
        private_directory(&directory)?;
        check_alias(&directory, "target.config")?;
        check_alias(&directory, "target.pending")?;
        write_atomic(&directory.join("target.config"), &bytes, replace)
    }
}

fn target_fault(mut fault: Fault, tab: &str, package: &str) -> Fault {
    fault.context["internal_name"] = json!(tab);
    fault.context["package_id"] = json!(package);
    fault.context["owner"] = json!("target");
    fault
}

#[cfg(test)]
mod tests;
