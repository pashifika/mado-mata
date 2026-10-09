use super::{LocalizedText, SetupItem, SetupLink, fault};
use mado_runtime_comparison::environment::{BOUNDED_PROFILE, G004_PROFILE};
use mado_runtime_comparison::model::{ENGINE_REVISION, Fault};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub(super) const LICENSE: &str = include_str!("../../resources/ocr-model-LICENSE.txt");
pub(super) const NOTICE: &str = include_str!("../../resources/ocr-model-NOTICE.txt");
const JSON: &str = include_str!("../../resources/ocr-catalog.json");
// The catalog may change transport, never the engine's accepted content.
const ACCEPTED: [(&str, u64, &str); 2] = [
    (
        "rapidocr-v3.9.2/ch_PP-OCRv4_det_mobile.onnx",
        4_745_517,
        "d2a7720d45a54257208b1e13e36a8479894cb74155a5efe29462512d42f49da9",
    ),
    (
        "rapidocr-v3.9.2/PP-OCRv6_rec_small.onnx",
        21_234_383,
        "6f327246b50388f3c176ae304bd95767ea6dc0c9ae92153ef8cbe210b3c14884",
    ),
];

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Catalog {
    pub schema: u32,
    pub engine_revision: String,
    pub profiles: Vec<String>,
    pub models: Models,
    pub platforms: Vec<Platform>,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Models {
    pub id: String,
    pub name: LocalizedText,
    pub detail: LocalizedText,
    pub version: String,
    pub license: String,
    pub license_url: String,
    pub links: Vec<SetupLink>,
    pub commands: Vec<String>,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Asset {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub url: String,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Platform {
    pub os: String,
    pub arch: String,
    pub runtime: Dependency,
    pub native: Dependency,
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Dependency {
    pub id: String,
    pub name: LocalizedText,
    pub detail: LocalizedText,
    pub version: String,
    pub license: String,
    pub license_url: String,
    pub links: Vec<SetupLink>,
    pub commands: Vec<String>,
    #[serde(default)]
    pub filename: String,
    #[serde(default)]
    pub bytes: u64,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub required_modules: Vec<String>,
}

impl Catalog {
    pub fn load() -> Result<Self, Fault> {
        let catalog: Self =
            serde_json::from_str(JSON).map_err(|error| fault("catalog", &error.to_string()))?;
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), Fault> {
        let profiles: BTreeSet<_> = self.profiles.iter().map(String::as_str).collect();
        if self.schema != 1
            || self.engine_revision != ENGINE_REVISION
            || profiles != BTreeSet::from([G004_PROFILE, BOUNDED_PROFILE])
            || self.profiles.len() != 2
            || self.models.id != "rapidocr-models"
            || self.models.version.is_empty()
            || self.models.license != "Apache-2.0"
            || self.models.assets.len() != ACCEPTED.len()
        {
            return Err(fault("catalog", "unsupported catalog/engine/model mapping"));
        }
        presentation(
            &self.models.name,
            &self.models.detail,
            &self.models.links,
            &self.models.commands,
        )?;
        https(&self.models.license_url)?;
        let mut paths = BTreeSet::new();
        for asset in &self.models.assets {
            safe_relative(&asset.path)?;
            https(&asset.url)?;
            if !paths.insert(&asset.path)
                || !ACCEPTED.contains(&(asset.path.as_str(), asset.bytes, asset.sha256.as_str()))
            {
                return Err(fault(
                    "catalog",
                    "model content is not accepted by the pinned engine",
                ));
            }
        }
        let mut targets = BTreeSet::new();
        for platform in &self.platforms {
            if !matches!(
                (platform.os.as_str(), platform.arch.as_str()),
                ("macos", "aarch64") | ("windows", "x86_64")
            ) || !targets.insert((&platform.os, &platform.arch))
                || platform.runtime.id != "onnxruntime"
                || platform.native.id != "native-libraries"
                || platform.runtime.version != "1.29.0-api17-cpu"
                || platform.runtime.bytes == 0
                || platform.runtime.bytes > 1_073_741_824
                || !sha256(&platform.runtime.sha256)
                || platform.native.required_modules.is_empty()
            {
                return Err(fault(
                    "catalog",
                    "unsupported or incomplete dependency mapping",
                ));
            }
            safe_relative(&platform.runtime.filename)?;
            if platform.runtime.filename.contains('/') {
                return Err(fault(
                    "catalog",
                    "runtime filename must be a single component",
                ));
            }
            for dependency in [&platform.runtime, &platform.native] {
                presentation(
                    &dependency.name,
                    &dependency.detail,
                    &dependency.links,
                    &dependency.commands,
                )?;
                if dependency.version.is_empty()
                    || dependency.license.is_empty()
                    || dependency.links.is_empty()
                {
                    return Err(fault(
                        "catalog",
                        "dependency needs version, license and guidance",
                    ));
                }
                https(&dependency.license_url)?;
            }
            let mut modules = BTreeSet::new();
            for module in &platform.native.required_modules {
                safe_relative(module)?;
                if module.contains('/') || !modules.insert(module.as_str()) {
                    return Err(fault("catalog", "invalid required native module"));
                }
            }
            let (runtime_name, native_version, required): (&str, &str, &[&str]) =
                if platform.os == "macos" {
                    (
                        "libonnxruntime.1.29.0.dylib",
                        "engine-linked-opencv-4",
                        &[
                            "libopencv_core.4",
                            "libopencv_imgproc.4",
                            "libopencv_imgcodecs.4",
                        ],
                    )
                } else {
                    (
                        "onnxruntime.dll",
                        "engine-linked-opencv-4.14.0",
                        &["opencv_world4140.dll"],
                    )
                };
            if platform.runtime.filename != runtime_name
                || platform.native.version != native_version
                || modules != required.iter().copied().collect()
            {
                return Err(fault(
                    "catalog",
                    "dependency names/version do not match the supported target",
                ));
            }
        }
        if targets.len() != 2 {
            return Err(fault(
                "catalog",
                "both declared guidance targets must be present",
            ));
        }
        Ok(())
    }

    pub fn platform(&self) -> Option<&Platform> {
        self.platforms.iter().find(|platform| {
            platform.os == std::env::consts::OS && platform.arch == std::env::consts::ARCH
        })
    }

    pub fn installation_name(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.schema.to_le_bytes());
        // Source/guidance edits do not duplicate the same accepted model set.
        for (path, bytes, digest) in ACCEPTED {
            hasher.update(path.as_bytes());
            hasher.update(bytes.to_le_bytes());
            hasher.update(digest.as_bytes());
        }
        format!("models-{:x}", hasher.finalize())
    }

    pub fn items(&self) -> Vec<SetupItem> {
        let mut items = vec![SetupItem {
            id: self.models.id.clone(),
            name: self.models.name.clone(),
            state: "missing".into(),
            detail: self.models.detail.clone(),
            downloadable: true,
            links: self.models.links.clone(),
            commands: self.models.commands.clone(),
        }];
        if let Some(platform) = self.platform() {
            items.extend([platform.runtime.item(), platform.native.item()]);
        } else {
            items.push(SetupItem {
                id: "unsupported-platform".into(),
                name: LocalizedText::new("Native prerequisites", "ネイティブ前提条件"),
                state: "unsupported".into(),
                detail: LocalizedText::new("Setup supports macOS arm64 and Windows x64 only; no environment can be proposed on this target.", "セットアップはmacOS arm64とWindows x64のみ対応しています。この環境では設定候補を作成できません。"),
                downloadable: false,
                links: Vec::new(),
                commands: Vec::new(),
            });
        }
        items
    }
}

impl Dependency {
    fn item(&self) -> SetupItem {
        SetupItem {
            id: self.id.clone(),
            name: self.name.clone(),
            state: "manual".into(),
            detail: self.detail.clone(),
            downloadable: false,
            links: self.links.clone(),
            commands: self.commands.clone(),
        }
    }
}

pub(super) fn safe_relative(path: &str) -> Result<(), Fault> {
    if path.is_empty()
        || path.contains(['\\', ':'])
        || path.chars().any(char::is_control)
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
        || Path::new(path)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(fault("catalog", "unsafe resource destination"));
    }
    Ok(())
}

fn https(url: &str) -> Result<(), Fault> {
    let request_url = url.split_once('#').map_or(url, |(base, _)| base);
    let uri: ureq::http::Uri = request_url
        .parse()
        .map_err(|_| fault("catalog", "invalid HTTPS URL"))?;
    if uri.scheme_str() != Some("https")
        || uri.host().is_none_or(str::is_empty)
        || uri
            .authority()
            .is_none_or(|authority| authority.as_str().contains('@'))
        || url.chars().any(char::is_control)
    {
        return Err(fault(
            "catalog",
            "resource links must be absolute HTTPS without credentials",
        ));
    }
    Ok(())
}

fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn presentation(
    name: &LocalizedText,
    detail: &LocalizedText,
    links: &[SetupLink],
    commands: &[String],
) -> Result<(), Fault> {
    if [&name.en, &name.ja, &detail.en, &detail.ja]
        .iter()
        .any(|text| text.trim().is_empty())
        || commands.iter().any(|command| {
            command.is_empty() || command.len() > 4096 || command.chars().any(char::is_control)
        })
    {
        return Err(fault(
            "catalog",
            "missing localized guidance or invalid display command",
        ));
    }
    for link in links {
        if link.label.en.trim().is_empty() || link.label.ja.trim().is_empty() {
            return Err(fault("catalog", "link requires localized labels"));
        }
        https(&link.url)?;
    }
    Ok(())
}
