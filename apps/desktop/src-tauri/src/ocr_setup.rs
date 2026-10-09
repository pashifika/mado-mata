//! Explicit resource setup. No engine loading, settings writes or ambient discovery.

mod catalog;
mod download;
mod files;
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

#[derive(Clone, Debug, Serialize)]
pub struct SetupView {
    pub items: Vec<SetupItem>,
    pub environment: Option<OcrEnvironment>,
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
    Ok(SetupView {
        items: Catalog::load()?.items(),
        environment: None,
    })
}

/// The caller holds exclusive setup admission until this call and cleanup settle.
pub fn inspect(
    root: &Path,
    environment: Option<&OcrEnvironment>,
    cancel: &AtomicBool,
) -> Result<SetupView, Fault> {
    let catalog = Catalog::load()?;
    inspect_with(&catalog, root, environment, cancel)
}

fn inspect_with(
    catalog: &Catalog,
    root: &Path,
    environment: Option<&OcrEnvironment>,
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
        });
    };
    let runtime = if proposal.runtime_path.is_empty() {
        Err(fault(
            "missing",
            "select the externally installed official runtime file",
        ))
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
    let native = files::native(&proposal.native_library_paths, platform, cancel);
    let native = observation(
        &mut items[2],
        native,
        "Required module names and target file headers checked. The supplied list remains your reviewed dependency closure; ABI/loadability are not certified. Save and Check.",
        "必須モジュール名と対象CPUのファイルヘッダーを確認しました。指定一覧の依存関係は利用者によるレビューが必要です。ABIとロード可否は保証しません。保存してCheckしてください。",
        cancel,
    )?;
    if let (Some(model_root), Some(runtime_path), Some(native_library_paths)) =
        (model_root, runtime, native)
    {
        proposal.model_root = model_root;
        proposal.runtime_path = runtime_path;
        proposal.native_library_paths = native_library_paths;
        proposal.validate()?;
        check_cancel(cancel)?;
        return Ok(SetupView {
            items,
            environment: Some(proposal),
        });
    }
    check_cancel(cancel)?;
    Ok(SetupView {
        items,
        environment: None,
    })
}

pub fn download(
    root: &Path,
    resource_id: &str,
    cancel: &AtomicBool,
    progress: impl FnMut(SetupProgress),
) -> Result<(), Fault> {
    let catalog = Catalog::load()?;
    if resource_id != catalog.models.id {
        return Err(fault(
            "resource",
            "only the catalog model set is downloadable; native prerequisites use manual guidance",
        ));
    }
    download::acquire(root, &catalog, cancel, progress)
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
