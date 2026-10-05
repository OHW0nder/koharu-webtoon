use std::path::PathBuf;

use anyhow::{Context as _, Result};
use koharu_desktop::{CanvasState, Desktop};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use strum::{EnumMessage as _, IntoEnumIterator as _};
use tauri::{AppHandle, Manager as _, State, WebviewWindow, ipc::Channel};
use tauri_runtime_cef::CefRuntime;

use super::{
    ChannelExt as _, Error,
    agent::AgentState,
    canvas::CanvasChannel,
    import::{self, Imported},
    preferences::Preferences,
    processing::{Job, JobChannel, Processing},
    project::{
        CurrentProject, Page, PageSummary, Project, ProjectInfo, ProjectLibrary, ProjectSummary,
    },
    reject_import_while_processing,
    series::{AdBands, SeriesLibrary},
};
use crate::webtoon;

#[derive(Clone, Debug, Serialize, Type)]
pub struct StartupState {
    pub preferences: Preferences,
    pub jobs: Vec<Job>,
    pub canvas: CanvasState,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct PageSelection {
    pub project: ProjectInfo,
    pub page: Page,
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PageImportSource {
    Files,
    Folder,
}

pub(crate) struct Initialization {
    ready: tokio::sync::watch::Sender<bool>,
}

impl Default for Initialization {
    fn default() -> Self {
        let (ready, _) = tokio::sync::watch::channel(false);
        Self { ready }
    }
}

impl Initialization {
    pub(crate) fn ready(&self) {
        self.ready.send_replace(true);
    }

    async fn wait(&self) -> Result<()> {
        let mut ready = self.ready.subscribe();
        while !*ready.borrow_and_update() {
            ready
                .changed()
                .await
                .context("startup state closed before initialization completed")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Download {
    #[specta(type = f64)]
    pub id: u64,
    pub state: DownloadState,
    pub name: Option<String>,
    #[specta(type = f64)]
    pub completed: u64,
    #[specta(type = f64)]
    pub total: u64,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct DownloadChannel {
    pub(crate) channel: Mutex<Option<Channel<Download>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Running,
    Finished,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct ModelResources {
    #[specta(type = f64)]
    pub process_memory: u64,
    #[specta(type = f64)]
    pub system_memory: u64,
    pub process_cpu: f32,
    pub devices: Vec<DeviceResources>,
}

#[derive(Default)]
pub(crate) struct ResourceChannel {
    pub(crate) channel: Mutex<Option<Channel<ModelResources>>>,
}

#[derive(Default)]
pub(crate) struct ProjectChannel {
    pub(crate) channel: Mutex<Option<Channel<Option<ProjectInfo>>>>,
}

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct DeviceResources {
    pub name: String,
    pub selected: bool,
    #[specta(type = Option<f64>)]
    pub memory_budget: Option<u64>,
    #[specta(type = Option<f64>)]
    pub memory_used: Option<u64>,
    pub utilization: Option<f32>,
}

impl From<koharu_pipeline::ResourceSnapshot> for ModelResources {
    fn from(value: koharu_pipeline::ResourceSnapshot) -> Self {
        Self {
            process_memory: value.process_memory_bytes,
            system_memory: value.system_memory_bytes,
            process_cpu: value.process_cpu_percent,
            devices: value
                .devices
                .into_iter()
                .map(|device| DeviceResources {
                    name: device.name,
                    selected: device.selected,
                    memory_budget: device.memory_budget_bytes,
                    memory_used: device.memory_used_bytes,
                    utilization: device.utilization_percent,
                })
                .collect(),
        }
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn subscribe(
    handle: AppHandle<CefRuntime>,
    on_canvas: Channel<CanvasState>,
    on_job: Channel<Job>,
    on_download: Channel<Download>,
    on_resources: Channel<ModelResources>,
    on_project: Channel<Option<ProjectInfo>>,
) -> std::result::Result<StartupState, Error> {
    handle.state::<Initialization>().wait().await?;

    *handle.state::<CanvasChannel>().channel.lock() = Some(on_canvas);
    *handle.state::<JobChannel>().channel.lock() = Some(on_job);
    *handle.state::<DownloadChannel>().channel.lock() = Some(on_download);
    *handle.state::<ResourceChannel>().channel.lock() = Some(on_resources);
    *handle.state::<ProjectChannel>().channel.lock() = Some(on_project);

    let canvas = handle.state::<Desktop>().canvas_state();
    let preferences = Preferences::load()?;
    Ok(StartupState {
        preferences,
        jobs: handle
            .state::<Processing>()
            .jobs
            .lock()
            .values()
            .cloned()
            .collect(),
        canvas,
    })
}

pub(crate) async fn replace_project(handle: &AppHandle<CefRuntime>, opened: Project) -> Result<()> {
    let snapshot = opened.snapshot();
    let page = opened.active_page();
    let info = opened.info();

    handle.state::<AgentState>().reset().await;
    let processing = handle.state::<Processing>();
    for stop in processing.stops.lock().values() {
        stop.stop();
    }
    processing.stops.lock().clear();
    processing.jobs.lock().clear();

    let previous = {
        let current = handle.state::<CurrentProject>();
        let mut current = current.project.lock().await;
        current.replace(opened)
    };

    let desktop = handle.state::<Desktop>();
    desktop.show_page(&snapshot, page).await?;
    let canvas = desktop.canvas_state();
    drop(previous);
    handle.state::<CanvasChannel>().channel.publish(canvas);
    handle.state::<ProjectChannel>().channel.publish(Some(info));
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_project(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Option<ProjectInfo>, Error> {
    Ok(project.project.lock().await.as_ref().map(Project::info))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_pages(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Vec<PageSummary>, Error> {
    let snapshot = project
        .project
        .lock()
        .await
        .as_ref()
        .context("no project is open")?
        .snapshot();
    Ok(Project::pages(&snapshot)?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_page(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Option<Page>, Error> {
    let current = {
        let project = project.project.lock().await;
        project
            .as_ref()
            .map(|project| (project.snapshot(), project.active_page()))
    };
    Ok(current
        .and_then(|(snapshot, page)| page.map(|page| (snapshot, page)))
        .map(|(snapshot, page)| Project::page(&snapshot, page))
        .transpose()?)
}

/// 列出没有任何漫画认领的项目，也就是漫画柜下方的「未分组项目」。
///
/// 认领关系由漫画索引决定，不由内核的项目枚举决定：内核只认项目，章项目在它眼里仍然是
/// 普通项目，能不能单独打开取决于有没有被登记成某一章。
#[tauri::command]
#[specta::specta]
pub(crate) async fn list_projects(
    library: State<'_, ProjectLibrary>,
    series: State<'_, SeriesLibrary>,
) -> std::result::Result<Vec<ProjectSummary>, Error> {
    let claimed = series.claimed_projects()?;
    Ok(library
        .list()?
        .into_iter()
        .filter(|project| !claimed.contains(&project.name))
        .collect())
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "project_created",
    skip_all,
    fields(origin = "user")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn create_project(
    name: String,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<(), Error> {
    let library = handle.state::<ProjectLibrary>().inner().clone();
    let opened = library.create(&name).await?;
    replace_project(&handle, opened).await?;
    Ok(())
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "project_opened",
    skip_all,
    fields(origin = "user")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn open_project(
    name: String,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<(), Error> {
    let library = handle.state::<ProjectLibrary>().inner().clone();
    let opened = library.open(&name).await?;
    replace_project(&handle, opened).await?;
    Ok(())
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "project_closed",
    skip_all,
    fields(origin = "user")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn close_project(handle: AppHandle<CefRuntime>) -> std::result::Result<(), Error> {
    close_current_project(&handle).await?;
    Ok(())
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "project_deleted",
    skip_all,
    fields(origin = "user")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_project(
    name: String,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<(), Error> {
    let active = handle
        .state::<CurrentProject>()
        .project
        .lock()
        .await
        .as_ref()
        .is_some_and(|project| project.name == name);
    if active {
        close_current_project(&handle).await?;
    }
    let library = handle.state::<ProjectLibrary>().inner().clone();
    tokio::task::spawn_blocking(move || library.delete(&name))
        .await
        .context("project deletion task failed")??;
    Ok(())
}

async fn close_current_project(handle: &AppHandle<CefRuntime>) -> Result<()> {
    handle.state::<AgentState>().reset().await;
    let processing = handle.state::<Processing>();
    for stop in processing.stops.lock().values() {
        stop.stop();
    }
    processing.stops.lock().clear();
    processing.jobs.lock().clear();
    let previous = {
        let current = handle.state::<CurrentProject>();
        let mut current = current.project.lock().await;
        current.take()
    };
    let desktop = handle.state::<Desktop>();
    desktop.clear().await;
    let result = desktop.canvas_state();
    drop(previous);
    handle.state::<CanvasChannel>().channel.publish(result);
    handle.state::<ProjectChannel>().channel.publish(None);
    Ok(())
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "import",
    skip_all,
    fields(origin = "user", method = ?source),
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn import(
    source: PageImportSource,
    window: WebviewWindow<CefRuntime>,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    processing: State<'_, Processing>,
    canvas_channel: State<'_, CanvasChannel>,
) -> std::result::Result<(), Error> {
    reject_import_while_processing(&processing)?;
    let Some(files) = select_import_paths(source, &window).await? else {
        return Ok(());
    };
    let imported = tokio_rayon::spawn(move || import::import(files)).await?;
    let page_count: usize = imported
        .iter()
        .map(|entry| match entry {
            import::Imported::Page(_) => 1,
            import::Imported::Strip { bands, .. } => bands.len(),
        })
        .sum();

    let (commit, page) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        let commit = import::apply(project, imported).await?;
        project.record(vec![commit.revision]);
        project.reconcile_page();
        let page = project.active_page();
        (commit, page)
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    let canvas = desktop.canvas_state();
    canvas_channel.channel.publish(canvas);
    tracing::info!(target: "koharu_metrics", metric = "page_imported", page_count);
    Ok(())
}

/// Opens the picker and returns the paths to import, or `None` when the user cancels.
/// Both import commands share this one picker, so the file filter, the recursion policy and the
/// "nothing importable was selected" failure are defined once.
async fn select_import_paths(
    source: PageImportSource,
    window: &WebviewWindow<CefRuntime>,
) -> Result<Option<Vec<PathBuf>>> {
    let extensions = import::Format::iter()
        .flat_map(|format| format.get_serializations())
        .collect::<Vec<_>>();
    let dialog = rfd::AsyncFileDialog::new()
        .add_filter("Images, archives, and PDF", &extensions)
        .set_parent(window);
    let files = match source {
        PageImportSource::Files => dialog.pick_files().await.map(|files| {
            files
                .into_iter()
                .map(|file| file.path().to_owned())
                .collect::<Vec<_>>()
        }),
        PageImportSource::Folder => dialog
            .pick_folder()
            .await
            .map(|folder| import::collect_importable(&folder.path()).unwrap_or_default()),
    };
    let Some(files) = files else {
        return Ok(None);
    };
    if files.is_empty() {
        anyhow::bail!("no supported images were found in the selection");
    }
    Ok(Some(files))
}

/// Imports a webtoon, dividing images that are far taller than they are wide into pages.
///
/// This is a path of its own rather than another parameter on the upstream import command. That
/// command keeps its signature, so the generated frontend protocol does not diverge from
/// upstream; the two paths share the picker, the commit and the canvas synchronization, and
/// differ only in how a tall image becomes pages.
#[tracing::instrument(
    level = "info",
    skip_all,
    fields(origin = "user", method = ?source, slicing = ?slicing),
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn import_webtoon(
    source: PageImportSource,
    slicing: Option<webtoon::PageImportSlicing>,
    window: WebviewWindow<CefRuntime>,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    processing: State<'_, Processing>,
    canvas_channel: State<'_, CanvasChannel>,
) -> std::result::Result<(), Error> {
    // Tauri deserializes command arguments field by field and never consults the argument type's
    // `Default`, so a required parameter fails every caller that omits it. Omitting this one means
    // the automatic geometry gate, which is what an ordinary webtoon import wants.
    let slicing = slicing.unwrap_or_default();
    reject_import_while_processing(&processing)?;
    let Some(files) = select_import_paths(source, &window).await? else {
        return Ok(());
    };
    // 这条命令不属于任何漫画，因此没有广告带设置可用——它不裁剪。条漫的正常入口是章节管理页，
    // 那里会从漫画设置里读广告带。
    let imported =
        tokio_rayon::spawn(move || import::import_webtoon(files, slicing.into(), AdBands::default()))
            .await?;
    let page_count: usize = imported
        .imported
        .iter()
        .map(|entry| match entry {
            Imported::Page(_) => 1,
            Imported::Strip { bands, .. } => bands.len(),
        })
        .sum();

    let (commit, page) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        let commit = import::apply(project, imported.imported).await?;
        project.record(vec![commit.revision]);
        project.reconcile_page();
        let page = project.active_page();
        (commit, page)
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    let canvas = desktop.canvas_state();
    canvas_channel.channel.publish(canvas);
    tracing::info!(target: "koharu_metrics", metric = "page_imported", page_count);
    Ok(())
}

impl From<webtoon::PageImportSlicing> for import::Slicing {
    fn from(value: webtoon::PageImportSlicing) -> Self {
        match value {
            webtoon::PageImportSlicing::Auto => Self::Auto,
            webtoon::PageImportSlicing::Forced => Self::Forced,
        }
    }
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "page_selected",
    skip_all,
    fields(origin = "user")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn select_page(
    desktop: State<'_, Desktop>,
    page: koharu_scene::EntityId,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> std::result::Result<PageSelection, Error> {
    let (snapshot, project_info, selected_page) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        project.select_page(page)?;
        let snapshot = project.snapshot();
        let project_info = project.info();
        let selected_page = Project::page(&snapshot, page)?;
        (snapshot, project_info, selected_page)
    };
    if desktop.show_page(&snapshot, Some(page)).await? {
        let canvas = desktop.canvas_state();
        canvas_channel.channel.publish(canvas);
    }
    Ok(PageSelection {
        project: project_info,
        page: selected_page,
    })
}
