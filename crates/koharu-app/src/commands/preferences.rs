use std::{fmt, path::PathBuf};

use anyhow::Result;
use koharu_pipeline::PipelineConfig;
use koharu_renderer::TypesettingConfig;
use koharu_secrets::ExposeSecret as _;
use koharu_translator::{Language, Model, Provider, ProviderConfig, ProvidersConfig};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager as _, State, WebviewWindow};
use tauri_runtime_cef::CefRuntime;

use super::{
    Error, Processing,
    project::{LibraryConfig, ProjectLibrary},
    reject_settings_while_processing,
    series::SeriesLibrary,
};

#[derive(Clone, Debug, Serialize, Type)]
pub struct Preferences {
    pub pipeline: PipelineConfig,
    pub providers: ProviderPreferences,
    pub typesetting: TypesettingConfig,
    pub languages: Vec<LanguageChoice>,
}

impl Preferences {
    pub(crate) fn load() -> Result<Self> {
        let pipeline = PipelineConfig::load()?;
        let providers = ProvidersConfig::load()?;
        let typesetting = TypesettingConfig::load()?;
        let pipeline = pipeline.read()?;
        let providers = providers.read()?;
        let typesetting = typesetting.read()?;
        Ok(Self {
            pipeline: pipeline.clone(),
            providers: ProviderPreferences::from_config(&providers)?,
            typesetting: typesetting.clone(),
            languages: Language::ALL
                .iter()
                .map(|language| LanguageChoice {
                    tag: language.tag().to_owned(),
                    name: language.to_string(),
                })
                .collect(),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ProviderPreferences {
    pub entries: Vec<ProviderPreference>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct ProviderPreference {
    pub name: String,
    pub config: ProviderConfig,
    pub credential: Option<CredentialInput>,
}

impl ProviderPreferences {
    fn from_config(config: &ProvidersConfig) -> Result<Self> {
        let entries = config
            .entries()
            .into_iter()
            .map(|config| {
                let provider = config.provider();
                let credential = if provider == Provider::Local {
                    None
                } else {
                    let key: &'static str = provider.into();
                    Some(CredentialInput::load(key)?)
                };
                Ok(ProviderPreference {
                    name: provider.name().to_owned(),
                    config,
                    credential,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self { entries })
    }

    fn into_config(self) -> Result<ProvidersConfig> {
        let mut configs = Vec::with_capacity(self.entries.len());
        let mut credentials = Vec::with_capacity(self.entries.len().saturating_sub(1));
        for entry in self.entries {
            let provider = entry.config.provider();
            match entry.credential {
                None if provider == Provider::Local => {}
                Some(credential) if provider != Provider::Local => {
                    let key: &'static str = provider.into();
                    credentials.push((key, credential));
                }
                None => anyhow::bail!("missing credential input for {provider}"),
                Some(_) => anyhow::bail!("local translation does not accept credentials"),
            }
            configs.push(entry.config);
        }
        let config = ProvidersConfig::from_entries(configs)?;
        for (key, credential) in credentials {
            credential.save(key)?;
        }
        Ok(config)
    }
}

#[derive(Clone, Default, Deserialize, Serialize, Type)]
pub struct CredentialInput {
    pub configured: bool,
    pub value: Option<String>,
    pub clear: bool,
}

impl fmt::Debug for CredentialInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialInput")
            .field("configured", &self.configured)
            .field("value", &self.value.as_ref().map(|_| "[REDACTED]"))
            .field("clear", &self.clear)
            .finish()
    }
}

impl CredentialInput {
    fn load(key: &str) -> Result<Self> {
        Ok(Self {
            configured: koharu_secrets::get(key)?
                .is_some_and(|secret| !secret.expose_secret().trim().is_empty()),
            value: None,
            clear: false,
        })
    }

    fn save(self, key: &str) -> Result<()> {
        if self.clear {
            koharu_secrets::delete(key)?;
        } else if let Some(value) = self.value {
            if value.trim().is_empty() {
                koharu_secrets::delete(key)?;
            } else {
                koharu_secrets::set(key, &value.into())?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct LanguageChoice {
    pub tag: String,
    pub name: String,
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "preferences_saved",
    skip_all,
    fields(setting = "application")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn save_preferences(
    handle: AppHandle<CefRuntime>,
    mut pipeline: PipelineConfig,
    providers: ProviderPreferences,
    typesetting: TypesettingConfig,
    processing: State<'_, Processing>,
) -> std::result::Result<Preferences, Error> {
    // 必须在碰凭据之前就拒绝：这个函数是整体替换管线配置，跑着的作业读的就是那份内存句柄。
    reject_settings_while_processing(&processing)?;
    remember_pipeline_profiles(&mut pipeline);
    let providers = providers.into_config()?;
    let pipeline_config = PipelineConfig::load()?;
    let providers_config = ProvidersConfig::load()?;
    let typesetting_config = TypesettingConfig::load()?;
    {
        let mut current = pipeline_config.write()?;
        *current = pipeline.clone();
        current.save()?;
    }
    {
        let mut current = providers_config.write()?;
        *current = providers;
        current.save()?;
    }
    {
        let mut current = typesetting_config.write()?;
        *current = typesetting;
        current.save()?;
    }
    let preferences = Preferences::load()?;
    // 管线订阅的是内存句柄而不是配置文件，所以设置保存之后必须显式推一次，否则要等下次启动才生效。
    // 走内存句柄换来的是漫画级的注入内容不落盘：两处各写各的，互不覆盖。
    publish_to_live_pipeline(&handle, pipeline);
    tracing::info!(
        target: "koharu_metrics",
        metric = "preference_changed",
        setting = "application",
    );
    Ok(preferences)
}

/// 把用户设置推给管线跑的那份内存句柄。推失败只记日志：设置已经落盘，重启后自然生效。
fn publish_to_live_pipeline(handle: &AppHandle<CefRuntime>, pipeline: PipelineConfig) {
    let live = handle.state::<koharu_config::Config<PipelineConfig>>();
    match live.write() {
        Ok(mut current) => *current = pipeline,
        Err(error) => tracing::error!(%error, "could not reach the live pipeline configuration"),
    }
}

/// 漫画库根目录，也就是界面上显示的那个位置。
///
/// 报的是磁盘上真正在用的目录，不是配置文件里那一份：用户没有指定时它是解析出来的默认值，而配置里
/// 并不存在那一项。
#[tauri::command]
#[specta::specta]
pub(crate) fn get_library_root(
    library: State<'_, ProjectLibrary>,
) -> std::result::Result<PathBuf, Error> {
    Ok(library.root().to_owned())
}

/// 指定漫画库根目录。
///
/// **只写配置，不搬数据。** 库在启动时解析一次位置并持有它，所以改完要重启才生效。因此库里已经装了
/// 漫画时直接拒绝：让用户以为换位置只是改个设置，重启后看到空库再重新导入一遍，比现在多问一句糟糕得多。
#[tauri::command]
#[specta::specta]
pub(crate) fn set_library_root(
    root: PathBuf,
    library: State<'_, ProjectLibrary>,
    series: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<(), Error> {
    reject_settings_while_processing(&processing)?;
    let current = library.root().to_owned();
    if root == current {
        return Ok(());
    }
    let held = series.list()?.len();
    if held > 0 {
        return Err(anyhow::anyhow!(
            "{current:?} still holds {held} series; move or delete them before pointing the library at {root:?}"
        )
        .into());
    }
    let config = LibraryConfig::load()?;
    {
        let mut live = config.write()?;
        live.root = Some(root.clone());
        live.save()?;
    }
    tracing::info!(library = %root.display(), "the library folder moves on the next start");
    Ok(())
}

/// 打开文件夹选择器，返回用户挑的位置，取消则返回 `None`。
///
/// 选择器留在后端，因为 `rfd` 需要窗口句柄；与章目录那个选择器是同一个理由，所以也是同一种写法。
#[tauri::command]
#[specta::specta]
pub(crate) async fn pick_library_folder(
    window: WebviewWindow<CefRuntime>,
) -> std::result::Result<Option<String>, Error> {
    Ok(rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
        .map(|folder| folder.path().to_string_lossy().into_owned()))
}

fn remember_pipeline_profiles(config: &mut PipelineConfig) {
    let koharu_pipeline::DetectionModel::KoharuLayoutRFDetrSeg2XL(settings) = &config.detection;
    config.processor.koharu_layout_rfdetr_seg_2xl = Some(settings.clone());
    if let koharu_pipeline::InpaintingModel::Flux2Klein(settings) = &config.inpainting {
        config.processor.flux2_klein = Some(settings.clone());
    }
    if let koharu_pipeline::InpaintingModel::RoremMixed(settings) = &config.inpainting {
        config.processor.rorem_mixed = Some(settings.clone());
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_preferences() -> std::result::Result<Preferences, Error> {
    Ok(Preferences::load()?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_translation_models() -> std::result::Result<Vec<Model>, Error> {
    Ok(koharu_translator::Translator::models().await?)
}
