//! Explicit resource setup. No engine loading, settings writes or ambient discovery.

mod catalog;
mod download;
mod files;
mod native;
mod runtime;

pub use native::NativeSelection;
#[cfg(test)]
mod tests;

use catalog::Catalog;
use mado_runtime_comparison::environment::{
    DEFAULT_OCR_PROFILE_ID, EvaluationOutcome, OCR_TARGET, OcrEnvironment, PROVIDER, RUNTIME_PROFILE,
    digest_hex, ocr_profile, ocr_profiles,
};
use mado_runtime_comparison::model::Fault;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LocalizedText {
    pub en: String,
    pub ja: String,
}

impl LocalizedText {
    fn new(en: impl Into<String>, ja: impl Into<String>) -> Self {
        Self {
            en: en.into(),
            ja: ja.into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SetupLink {
    pub label: LocalizedText,
    pub url: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupItem {
    pub id: String,
    pub name: LocalizedText,
    pub state: String,
    pub detail: LocalizedText,
    pub downloadable: bool,
    pub links: Vec<SetupLink>,
    pub commands: Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ResolvedResources {
    pub model_root: Option<String>,
    pub runtime_path: Option<String>,
    pub native_library_paths: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupView {
    pub items: Vec<SetupItem>,
    pub environment: Option<OcrEnvironment>,
    pub resolved: ResolvedResources,
    pub native_methods: &'static [&'static str],
    pub profiles: Vec<OcrProfileChoice>,
    pub default_profile: &'static str,
    pub target: &'static str,
    pub profile: &'static str,
    pub resource_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct OcrProfileChoice {
    pub profile: &'static str,
    pub model: &'static str,
    pub language: &'static str,
    pub provider: &'static str,
    pub runtime_profile: &'static str,
    pub preprocessing: &'static str,
    pub display_name: &'static str,
    pub variant: Option<&'static str>,
    pub resource_id: String,
    pub evaluations: Vec<ProfileEvaluation>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileEvaluation {
    pub target: &'static str,
    pub provider: &'static str,
    pub criteria: &'static str,
    pub evidence: Option<ProfileEvidence>,
    pub rules: Vec<ProfileRule>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileEvidence {
    pub report: &'static str,
    pub measured_source_revision: &'static str,
    pub execution_sha256: String,
    pub evaluator_revision: &'static str,
}

#[derive(Clone, Debug, Serialize)]
pub struct ProfileRule {
    pub rule_id: &'static str,
    pub outcome: &'static str,
}

fn profile_choices() -> Vec<OcrProfileChoice> {
    ocr_profiles()
        .map(|profile| OcrProfileChoice {
            profile: profile.profile_id,
            model: profile.model_id,
            language: profile.language_profile_id,
            provider: PROVIDER,
            runtime_profile: RUNTIME_PROFILE,
            preprocessing: profile.preprocessing_id,
            display_name: profile.display_name,
            variant: profile.variant,
            resource_id: catalog::model_resource_id(profile),
            evaluations: profile
                .evaluations
                .iter()
                .map(|summary| ProfileEvaluation {
                    target: summary.target,
                    provider: summary.provider,
                    criteria: summary.criteria,
                    evidence: summary.evidence.map(|evidence| ProfileEvidence {
                        report: evidence.report,
                        measured_source_revision: evidence.measured_source_revision,
                        execution_sha256: digest_hex(&evidence.execution_sha256),
                        evaluator_revision: evidence.evaluator_revision,
                    }),
                    rules: summary
                        .rules
                        .iter()
                        .map(|rule| ProfileRule {
                            rule_id: rule.rule_id,
                            outcome: match rule.outcome {
                                EvaluationOutcome::Passed => "passed",
                                EvaluationOutcome::Failed => "failed",
                                EvaluationOutcome::NotRun => "not_run",
                                EvaluationOutcome::Incomplete => "incomplete",
                            },
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect()
}

#[derive(Clone, Debug, Serialize)]
pub struct SetupProgress {
    pub stage: String,
    pub resource_id: String,
    pub bytes: u64,
    pub total: u64,
}

/// Pure embedded catalog presentation; never touches local files or the network.
pub fn catalog_view() -> Result<SetupView, Fault> {
    let catalog = Catalog::load()?;
    Ok(view_for(&catalog))
}

fn view_for(catalog: &Catalog) -> SetupView {
    SetupView {
        items: catalog.items(),
        environment: None,
        resolved: ResolvedResources::default(),
        native_methods: native_methods(catalog),
        profiles: profile_choices(),
        default_profile: DEFAULT_OCR_PROFILE_ID,
        target: OCR_TARGET,
        profile: catalog.profile.profile_id,
        resource_id: catalog.models.id.clone(),
    }
}

pub(crate) fn validate_request(
    profile: &str,
    resource_id: &str,
    environment: Option<&OcrEnvironment>,
) -> Result<(), Fault> {
    let catalog = Catalog::for_profile(profile)?;
    proposed_tuple(profile, environment)?;
    validate_resource(&catalog, resource_id)
}

fn validate_resource(catalog: &Catalog, resource_id: &str) -> Result<(), Fault> {
    if resource_id != "inspect"
        && resource_id != catalog.models.id
        && !(resource_id == "onnxruntime" && catalog.platform().is_some())
    {
        return Err(fault(
            "resource",
            "resource does not belong to the selected profile",
        ));
    }
    Ok(())
}

fn native_methods(catalog: &Catalog) -> &'static [&'static str] {
    match catalog.platform().map(|platform| platform.os.as_str()) {
        Some("macos") => &["homebrew", "folders"],
        Some("windows") => &["folders"],
        _ => &[],
    }
}

/// The caller holds exclusive setup admission until this call and cleanup settle.
pub fn inspect(
    root: &Path,
    profile: &str,
    environment: Option<&OcrEnvironment>,
    native_selection: Option<&NativeSelection>,
    cancel: &AtomicBool,
) -> Result<SetupView, Fault> {
    let catalog = Catalog::for_profile(profile)?;
    inspect_with(&catalog, root, environment, native_selection, cancel)
}

fn inspect_with(
    catalog: &Catalog,
    root: &Path,
    environment: Option<&OcrEnvironment>,
    native_selection: Option<&NativeSelection>,
    cancel: &AtomicBool,
) -> Result<SetupView, Fault> {
    check_cancel(cancel)?;
    let mut proposal = proposed_tuple(catalog.profile.profile_id, environment)?;
    let _lease = download::recover(root, catalog)?;
    let mut view = view_for(catalog);
    let items = &mut view.items;
    let managed = proposal.model_root.is_empty();
    let mut model_root = if managed {
        root.join("ocr-resources").join(catalog.installation_name())
    } else {
        PathBuf::from(&proposal.model_root)
    };
    if managed && !model_root.is_absolute() {
        model_root = std::path::absolute(model_root).map_err(|error| io_fault("path", error))?;
    }
    let models = files::models(&model_root, catalog, cancel).and_then(|()| {
        if managed {
            download::verify_receipt(&model_root, catalog)?;
        }
        absolute_string(&model_root)
    });
    let model_root = observation(
        selected_item(items, &catalog.models.id)?,
        models,
        "Exact selected model resources verified. Engine initialization still requires Check.",
        "選択したモデルの全リソースの完全一致を検証しました。エンジン初期化はCheckで確認してください。",
        cancel,
    )?;
    let Some(platform) = catalog.platform() else {
        view.resolved.model_root = model_root;
        return Ok(view);
    };
    let runtime = if proposal.runtime_path.is_empty() {
        runtime::installed(root, catalog, cancel)
    } else {
        files::runtime(Path::new(&proposal.runtime_path), &platform.runtime, cancel)
    };
    let runtime = observation(
        selected_item(items, &platform.runtime.id)?,
        runtime,
        "Reviewed official runtime bytes verified; API/initialization still require Check.",
        "レビュー済み公式ランタイムのバイト列を検証しました。APIと初期化はCheckで確認してください。",
        cancel,
    )?;
    let native = match native_selection {
        Some(selection) => native::discover(platform, selection, cancel),
        None => files::native(&proposal.native_library_paths, platform, cancel),
    };
    let native = observation(
        selected_item(items, &platform.native.id)?,
        native,
        "Image-processing library files checked. Save settings; engine initialization is checked separately.",
        "画像処理ライブラリのファイルを確認しました。設定を保存してください。エンジン初期化の確認は別の操作です。",
        cancel,
    )?;
    let resolved = ResolvedResources {
        model_root,
        runtime_path: runtime,
        native_library_paths: native,
    };
    let environment = if let (Some(model_root), Some(runtime_path), Some(native_library_paths)) = (
        &resolved.model_root,
        &resolved.runtime_path,
        &resolved.native_library_paths,
    ) {
        proposal.model_root.clone_from(model_root);
        proposal.runtime_path.clone_from(runtime_path);
        proposal
            .native_library_paths
            .clone_from(native_library_paths);
        proposal.validate()?;
        Some(proposal)
    } else {
        None
    };
    check_cancel(cancel)?;
    view.environment = environment;
    view.resolved = resolved;
    Ok(view)
}

pub fn download(
    root: &Path,
    profile: &str,
    resource_id: &str,
    cancel: &AtomicBool,
    progress: impl FnMut(SetupProgress),
) -> Result<SetupView, Fault> {
    let catalog = Catalog::for_profile(profile)?;
    validate_resource(&catalog, resource_id)?;
    let mut view = view_for(&catalog);
    if resource_id == catalog.models.id {
        download::acquire(root, &catalog, cancel, progress)?;
        view.resolved.model_root = Some(absolute_string(
            &root.join("ocr-resources").join(catalog.installation_name()),
        )?);
        selected_item(&mut view.items, resource_id)?.state = "verified".into();
    } else if resource_id == "onnxruntime" {
        view.resolved.runtime_path = Some(runtime::acquire(root, &catalog, cancel, progress)?);
        selected_item(&mut view.items, resource_id)?.state = "verified".into();
    } else {
        return Err(fault("resource", "select a downloadable catalog resource"));
    }
    Ok(view)
}

pub fn guidance_link(resource_id: &str, index: usize) -> Result<String, Fault> {
    catalog_view()?
        .items
        .into_iter()
        .find(|item| item.id == resource_id)
        .and_then(|item| item.links.get(index).map(|link| link.url.clone()))
        .ok_or_else(|| fault("resource", "unknown catalog guidance link"))
}

pub fn guidance_command(resource_id: &str, index: usize) -> Result<String, Fault> {
    catalog_view()?
        .items
        .into_iter()
        .find(|item| item.id == resource_id)
        .and_then(|item| item.commands.get(index).cloned())
        .ok_or_else(|| fault("resource", "unknown catalog guidance command"))
}

fn proposed_tuple(profile: &str, hints: Option<&OcrEnvironment>) -> Result<OcrEnvironment, Fault> {
    let descriptor = ocr_profile(profile)?;
    let mut proposal = OcrEnvironment {
        model: descriptor.model_id.into(),
        profile: descriptor.profile_id.into(),
        language: descriptor.language_profile_id.into(),
        provider: PROVIDER.into(),
        runtime_profile: RUNTIME_PROFILE.into(),
        model_root: String::new(),
        runtime_path: String::new(),
        native_library_paths: Vec::new(),
    };
    if let Some(hints) = hints {
        for (supplied, required) in [
            (&hints.profile, descriptor.profile_id),
            (&hints.model, descriptor.model_id),
            (&hints.language, descriptor.language_profile_id),
            (&hints.provider, PROVIDER),
            (&hints.runtime_profile, RUNTIME_PROFILE),
        ] {
            if !supplied.is_empty() && supplied != required {
                return Err(fault(
                    "unsupported",
                    "the supplied model/language/provider/runtime tuple is unsupported",
                ));
            }
        }
        proposal.model_root.clone_from(&hints.model_root);
        proposal.runtime_path.clone_from(&hints.runtime_path);
        proposal
            .native_library_paths
            .clone_from(&hints.native_library_paths);
    }
    Ok(proposal)
}

fn selected_item<'a>(items: &'a mut [SetupItem], id: &str) -> Result<&'a mut SetupItem, Fault> {
    items
        .iter_mut()
        .find(|item| item.id == id)
        .ok_or_else(|| fault("catalog", "selected resource has no setup item"))
}

fn observation<T>(
    item: &mut SetupItem,
    result: Result<T, Fault>,
    en: &str,
    ja: &str,
    cancel: &AtomicBool,
) -> Result<Option<T>, Fault> {
    check_cancel(cancel)?;
    match result {
        Ok(value) => {
            item.state = "verified".into();
            item.detail = LocalizedText::new(en, ja);
            Ok(Some(value))
        }
        Err(error) => {
            item.state = if error.context["stage"] == "missing" {
                "missing"
            } else {
                "incompatible"
            }
            .into();
            item.detail = LocalizedText::new(
                format!("{} — {}", error.message, item.detail.en),
                format!("確認未完了: {} — {}", error.message, item.detail.ja),
            );
            Ok(None)
        }
    }
}

fn absolute_string(path: &Path) -> Result<String, Fault> {
    let path = path
        .canonicalize()
        .map_err(|error| io_fault("path", error))?;
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| fault("path", "resource path is not UTF-8"))
}

fn check_cancel(cancel: &AtomicBool) -> Result<(), Fault> {
    if cancel.load(Ordering::Acquire) {
        Err(Fault::new(
            "OcrSetupCancelled",
            "OCR resource setup was cancelled",
        ))
    } else {
        Ok(())
    }
}

fn fault(stage: &str, message: &str) -> Fault {
    Fault::new("OcrSetup", message).with_context(json!({"stage": stage}))
}

fn io_fault(stage: &str, error: std::io::Error) -> Fault {
    fault(
        if error.kind() == std::io::ErrorKind::NotFound {
            "missing"
        } else {
            stage
        },
        &error.to_string(),
    )
}
