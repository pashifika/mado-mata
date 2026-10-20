use super::{LocalizedText, SetupItem, SetupLink, fault};
use mado_runtime_comparison::environment::{
    DEFAULT_OCR_PROFILE_ID, OcrProfileDescriptor, digest_hex, model_resources, ocr_profile,
    ocr_profiles,
};
use mado_runtime_comparison::model::{ENGINE_REVISION, Fault};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Component, Path};

pub(super) const LICENSE: &str = include_str!("../../resources/ocr-model-LICENSE.txt");
pub(super) const NOTICE: &str = include_str!("../../resources/ocr-model-NOTICE.txt");
const JSON: &str = include_str!("../../resources/ocr-catalog.json");

#[derive(Clone)]
pub(super) struct Catalog {
    pub schema: u32,
    pub engine_revision: String,
    pub profile: &'static OcrProfileDescriptor,
    pub models: Models,
    pub platforms: Vec<Platform>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Dependencies {
    schema: u32,
    engine_revision: String,
    platforms: Vec<Platform>,
}

#[derive(Clone)]
pub(super) struct Models {
    pub id: String,
    pub name: LocalizedText,
    pub detail: LocalizedText,
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
    #[serde(default)]
    pub search_roots: Vec<String>,
    #[serde(default)]
    pub search_subdirectories: Vec<String>,
    #[serde(default)]
    pub archive: Option<RuntimeArchive>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ArchiveFormat {
    Tgz,
    Zip,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeArchive {
    pub format: ArchiveFormat,
    pub asset: Asset,
    pub members: Vec<RuntimeMember>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct RuntimeMember {
    pub archive_path: String,
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

impl Catalog {
    pub fn load() -> Result<Self, Fault> {
        Self::for_profile(DEFAULT_OCR_PROFILE_ID)
    }

    pub fn for_profile(id: &str) -> Result<Self, Fault> {
        let profile = ocr_profile(id)?;
        let dependencies: Dependencies =
            serde_json::from_str(JSON).map_err(|error| fault("catalog", &error.to_string()))?;
        let catalog = Self {
            schema: dependencies.schema,
            engine_revision: dependencies.engine_revision,
            profile,
            models: Models::from_profile(profile),
            platforms: dependencies.platforms,
        };
        catalog.validate()?;
        Ok(catalog)
    }

    pub fn validate(&self) -> Result<(), Fault> {
        let resources = model_resources(self.profile);
        if self.schema != 1
            || self.engine_revision != ENGINE_REVISION
            || ocr_profile(self.profile.profile_id)? != self.profile
            || self.models.id != model_resource_id(self.profile)
            || self.models.assets.len() != 2 + usize::from(self.profile.auxiliary.is_some())
        {
            return Err(fault("catalog", "unsupported catalog/engine/model mapping"));
        }
        presentation(
            &self.models.name,
            &self.models.detail,
            &self.models.links,
            &self.models.commands,
        )?;
        let mut paths = BTreeSet::new();
        for (asset, resource) in self.models.assets.iter().zip(resources) {
            safe_relative(&asset.path)?;
            https(&asset.url)?;
            if !paths.insert(&asset.path)
                || asset.path != resource.relative_path
                || asset.bytes != resource.byte_len
                || asset.sha256 != digest_hex(&resource.sha256)
                || asset.url != resource.source_url
                || resource.license != "Apache-2.0"
                || resource.notice.is_empty()
                || resource.source_revision.is_empty()
            {
                return Err(fault("catalog", "model resource differs from the pinned SDK"));
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
            platform.validate_acquisition()?;
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
        installation_name(self.schema, &self.models.assets)
    }

    pub fn notice(&self) -> String {
        use std::fmt::Write;
        let mut text = String::from(NOTICE);
        writeln!(
            &mut text,
            "\nSelected SDK resources at {}:",
            self.engine_revision
        )
        .expect("writing to a String");
        for resource in model_resources(self.profile) {
            writeln!(
                &mut text,
                "\n{}\n  Source: {}\n  Revision: {}\n  License: {}\n  Notice: {}",
                resource.relative_path,
                resource.source_url,
                resource.source_revision,
                resource.license,
                resource.notice
            )
            .expect("writing to a String");
        }
        text
    }

    pub fn items(&self) -> Vec<SetupItem> {
        let mut items = vec![self.models.item()];
        for profile in ocr_profiles() {
            let id = model_resource_id(profile);
            if !items.iter().any(|item| item.id == id) {
                items.push(Models::from_profile(profile).item());
            }
        }
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

pub(super) fn model_resource_id(profile: &OcrProfileDescriptor) -> String {
    let default = ocr_profile(DEFAULT_OCR_PROFILE_ID).expect("SDK default is declared");
    let identity = |resource: &mado_runtime_comparison::environment::ResourceDescriptor| {
        (resource.relative_path, resource.byte_len, resource.sha256)
    };
    if model_resources(profile)
        .map(identity)
        .eq(model_resources(default).map(identity))
    {
        "rapidocr-models".into()
    } else {
        let mut hasher = Sha256::new();
        hasher.update(1_u32.to_le_bytes());
        for resource in model_resources(profile) {
            hasher.update(resource.relative_path.as_bytes());
            hasher.update(resource.byte_len.to_le_bytes());
            hasher.update(digest_hex(&resource.sha256).as_bytes());
        }
        format!("models-{:x}", hasher.finalize())
    }
}

fn installation_name(schema: u32, assets: &[Asset]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(schema.to_le_bytes());
    // Preserve the published v1 identity, independent of URLs and SDK revision.
    for asset in assets {
        hasher.update(asset.path.as_bytes());
        hasher.update(asset.bytes.to_le_bytes());
        hasher.update(asset.sha256.as_bytes());
    }
    format!("models-{:x}", hasher.finalize())
}

impl Asset {
    fn from_resource(resource: &mado_runtime_comparison::environment::ResourceDescriptor) -> Self {
        Self {
            path: resource.relative_path.into(),
            bytes: resource.byte_len,
            sha256: digest_hex(&resource.sha256),
            url: resource.source_url.into(),
        }
    }
}

impl Models {
    fn from_profile(profile: &OcrProfileDescriptor) -> Self {
        Self {
            id: model_resource_id(profile),
            name: LocalizedText::new(profile.display_name, profile.display_name),
            detail: LocalizedText::new(
                "Download the selected model's complete resource set. Cancelling settings keeps downloaded files. Save and Check separately to test engine initialization.",
                "選択したモデルの全リソースを取得します。設定をキャンセルしても取得済みファイルは残ります。保存後のCheckでエンジン初期化を確認してください。",
            ),
            links: vec![
                SetupLink {
                    label: LocalizedText::new(
                        "SDK resource identities and provenance",
                        "SDKリソースの識別情報と出典",
                    ),
                    url: format!("https://github.com/pashifika/mado-pilot/blob/{ENGINE_REVISION}/crates/automation/ocr/resources/model-catalog.json"),
                },
                SetupLink {
                    label: LocalizedText::new("Apache-2.0 license", "Apache-2.0ライセンス"),
                    url: "https://www.apache.org/licenses/LICENSE-2.0".into(),
                },
                SetupLink {
                    label: LocalizedText::new(
                        "Hugging Face download terms",
                        "Hugging Faceの利用規約",
                    ),
                    url: "https://huggingface.co/terms-of-service".into(),
                },
            ],
            commands: Vec::new(),
            assets: model_resources(profile).map(Asset::from_resource).collect(),
        }
    }

    fn item(&self) -> SetupItem {
        SetupItem {
            id: self.id.clone(),
            name: self.name.clone(),
            state: "not_checked".into(),
            detail: self.detail.clone(),
            downloadable: true,
            links: self.links.clone(),
            commands: self.commands.clone(),
        }
    }
}

impl Dependency {
    fn item(&self) -> SetupItem {
        SetupItem {
            id: self.id.clone(),
            name: self.name.clone(),
            state: if self.archive.is_some() {
                "not_checked"
            } else {
                "manual"
            }
            .into(),
            detail: self.detail.clone(),
            downloadable: self.archive.is_some(),
            links: self.links.clone(),
            commands: self.commands.clone(),
        }
    }
}

impl Platform {
    fn validate_acquisition(&self) -> Result<(), Fault> {
        let (
            prefix,
            format,
            bytes,
            digest,
            runtime_bytes,
            runtime_digest,
            license_bytes,
            license_digest,
            notice_bytes,
            notice_digest,
        ) = if self.os == "macos" {
            (
                "onnxruntime-osx-arm64-1.29.0",
                ArchiveFormat::Tgz,
                41_578_864,
                "d0706fc34f315d8c88639d0a8c81f2e09e815f282cabed3493c06a054352cf92",
                43_184_400,
                "68f6e54e695583adc371aef610ec4abb1ffaa3df656582922de7690f7e2000eb",
                1_073,
                "2f07c72751aed99790b8a4869cf2311df85a860b22ded05fa22803587a48922c",
                336_906,
                "53d3fa5821ac016ac24dd35775c996efec86e2ae0841e9a3a5e146c0ae916845",
            )
        } else {
            (
                "onnxruntime-win-x64-1.29.0",
                ArchiveFormat::Zip,
                79_645_520,
                "c9b4b7086b529ad814f428c1bad028e20a25d7dc0699836775faace4ab5b78b2",
                16_149_344,
                "69d8e6d3879a3b4001cdc74c8ed9ccc7e7f799a5b847059738323404519ec471",
                1_094,
                "c250d6278f0b47a6439fb7592b08b58a55eb9f535aa49a1db63211c3f982b674",
                343_249,
                "4c5b864d8974c94b37461f38163facef79a1bb5dea461667ee9e5be6a8e73f83",
            )
        };
        let archive = self
            .runtime
            .archive
            .as_ref()
            .ok_or_else(|| fault("catalog", "runtime requires a reviewed archive"))?;
        let extension = if format == ArchiveFormat::Tgz {
            "tgz"
        } else {
            "zip"
        };
        if archive.format != format
            || archive.asset.path != format!("{prefix}.{extension}")
            || archive.asset.bytes != bytes
            || archive.asset.sha256 != digest
            || archive.members.len() != 3
            || self.runtime.bytes != runtime_bytes
            || self.runtime.sha256 != runtime_digest
            || self.runtime.license != "MIT"
            || self.native.archive.is_some()
            || !self.runtime.search_roots.is_empty()
            || !self.runtime.search_subdirectories.is_empty()
        {
            return Err(fault(
                "catalog",
                "runtime archive differs from the reviewed release",
            ));
        }
        https(&archive.asset.url)?;
        let expected = [
            (
                format!("lib/{}", self.runtime.filename),
                self.runtime.filename.as_str(),
                runtime_bytes,
                runtime_digest,
            ),
            ("LICENSE".into(), "LICENSE", license_bytes, license_digest),
            (
                "ThirdPartyNotices.txt".into(),
                "ThirdPartyNotices.txt",
                notice_bytes,
                notice_digest,
            ),
        ];
        let mut destinations = BTreeSet::new();
        for member in &archive.members {
            safe_relative(&member.archive_path)?;
            safe_relative(&member.path)?;
            if !destinations.insert(&member.path)
                || !expected.iter().any(|(source, path, bytes, digest)| {
                    member.archive_path == format!("{prefix}/{source}")
                        && member.path == *path
                        && member.bytes == *bytes
                        && member.sha256 == *digest
                })
            {
                return Err(fault(
                    "catalog",
                    "runtime members differ from the reviewed release",
                ));
            }
        }
        let roots: &[&str] = if self.os == "macos" {
            &["/opt/homebrew", "/usr/local"]
        } else {
            &[]
        };
        if self.native.search_roots.len() > 8
            || self.native.search_subdirectories.len() > 8
            || self
                .native
                .search_roots
                .iter()
                .any(|root| !roots.contains(&root.as_str()))
            || self
                .native
                .search_roots
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != self.native.search_roots.len()
        {
            return Err(fault(
                "catalog",
                "native search roots must be bounded known absolute roots",
            ));
        }
        let mut subdirectories = BTreeSet::new();
        for subdirectory in &self.native.search_subdirectories {
            safe_relative(subdirectory)?;
            if subdirectory.len() > 256 || !subdirectories.insert(subdirectory) {
                return Err(fault("catalog", "invalid native search subdirectories"));
            }
        }
        Ok(())
    }

    pub fn runtime_installation_name(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.os.as_bytes());
        hasher.update(self.arch.as_bytes());
        if let Some(archive) = &self.runtime.archive {
            for member in &archive.members {
                hasher.update(member.path.as_bytes());
                hasher.update(member.bytes.to_le_bytes());
                hasher.update(member.sha256.as_bytes());
            }
        }
        format!("runtime-{:x}", hasher.finalize())
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
