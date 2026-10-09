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
    BOUNDED_PROFILE, G004_PROFILE, LANGUAGE, OcrEnvironment, PROVIDER, RUNTIME_PROFILE,
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
    Ok(SetupView {
        items: catalog.items(),
        environment: None,
        resolved: ResolvedResources::default(),
        native_methods: native_methods(&catalog),
    })
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
    environment: Option<&OcrEnvironment>,
    native_selection: Option<&NativeSelection>,
    cancel: &AtomicBool,
) -> Result<SetupView, Fault> {
    let catalog = Catalog::load()?;
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
    let mut proposal = proposed_tuple(environment)?;
    let _lease = download::recover(root, catalog)?;
    let mut items = catalog.items();
    let managed = proposal.model_root.is_empty();
    let model_root = if managed {
        root.join("ocr-resources").join(catalog.installation_name())
    } else {
        PathBuf::from(&proposal.model_root)
    };
    let models = files::models(&model_root, catalog, cancel).and_then(|()| {
        if managed {
            download::verify_receipt(&model_root, catalog)?;
        }
        absolute_string(&model_root)
    });
    let model_root = observation(
        &mut items[0],
        models,
        "Exact accepted model bytes verified.",
        "対応するモデルの完全一致を検証しました。",
        cancel,
    )?;
    let Some(platform) = catalog.platform() else {
        return Ok(SetupView {
            items,
            environment: None,
            resolved: ResolvedResources {
                model_root,
                ..ResolvedResources::default()
            },
            native_methods: native_methods(catalog),
        });
    };
    let runtime = if proposal.runtime_path.is_empty() {
        runtime::installed(root, catalog, cancel)
    } else {
        files::runtime(Path::new(&proposal.runtime_path), &platform.runtime, cancel)
    };
    let runtime = observation(
        &mut items[1],
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
        &mut items[2],
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
    Ok(SetupView {
        items,
        environment,
        resolved,
        native_methods: native_methods(catalog),
    })
}

pub fn download(
    root: &Path,
    resource_id: &str,
    cancel: &AtomicBool,
    progress: impl FnMut(SetupProgress),
) -> Result<SetupView, Fault> {
    let catalog = Catalog::load()?;
    let mut view = SetupView {
        items: catalog.items(),
        environment: None,
        resolved: ResolvedResources::default(),
        native_methods: native_methods(&catalog),
    };
    match resource_id {
        "rapidocr-models" => {
            download::acquire(root, &catalog, cancel, progress)?;
            view.resolved.model_root = Some(absolute_string(
                &root.join("ocr-resources").join(catalog.installation_name()),
            )?);
            view.items[0].state = "verified".into();
        }
        "onnxruntime" => {
            view.resolved.runtime_path = Some(runtime::acquire(root, &catalog, cancel, progress)?);
            view.items[1].state = "verified".into();
        }
        _ => return Err(fault("resource", "select a downloadable catalog resource")),
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

fn proposed_tuple(hints: Option<&OcrEnvironment>) -> Result<OcrEnvironment, Fault> {
    let profile = hints.map_or("", |value| value.profile.as_str());
    let profile = if profile.is_empty() {
        G004_PROFILE
    } else {
        profile
    };
    if !matches!(profile, G004_PROFILE | BOUNDED_PROFILE) {
        return Err(fault(
            "unsupported",
            "the supplied OCR profile is unsupported; no fallback is allowed",
        ));
    }
    let mut proposal = OcrEnvironment {
        model: profile.into(),
        profile: profile.into(),
        language: LANGUAGE.into(),
        provider: PROVIDER.into(),
        runtime_profile: RUNTIME_PROFILE.into(),
        model_root: String::new(),
        runtime_path: String::new(),
        native_library_paths: Vec::new(),
    };
    if let Some(hints) = hints {
        for (supplied, required) in [
            (&hints.model, profile),
            (&hints.language, LANGUAGE),
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
