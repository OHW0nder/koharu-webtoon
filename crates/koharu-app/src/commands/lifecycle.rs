use anyhow::{Context as _, Result};
use koharu_desktop::{CanvasState, Desktop};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager as _, State, ipc::Channel};
use tauri_runtime_cef::CefRuntime;

use super::{
    ChannelExt as _, Error,
    agent::AgentState,
    canvas::CanvasChannel,
    preferences::Preferences,
    processing::{Job, JobChannel, Processing},
    project::{ChapterRef, CurrentProject, Page, PageSummary, Project, ProjectInfo, ProjectLibrary},
    series::SeriesLibrary,
    source::{SourceFetch, SourceFetchChannel},
};

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
    #[specta(type = f32)]
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
    on_source_fetch: Channel<SourceFetch>,
) -> std::result::Result<StartupState, Error> {
    handle.state::<Initialization>().wait().await?;

    *handle.state::<CanvasChannel>().channel.lock() = Some(on_canvas);
    *handle.state::<JobChannel>().channel.lock() = Some(on_job);
    *handle.state::<DownloadChannel>().channel.lock() = Some(on_download);
    *handle.state::<ResourceChannel>().channel.lock() = Some(on_resources);
    *handle.state::<ProjectChannel>().channel.lock() = Some(on_project);
    *handle.state::<SourceFetchChannel>().channel.lock() = Some(on_source_fetch);

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

#[tracing::instrument(
    target = "koharu_metrics",
    name = "project_opened",
    skip_all,
    fields(origin = "user")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn open_chapter(
    reference: ChapterRef,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<(), Error> {
    let library = handle.state::<ProjectLibrary>().inner().clone();
    let series = handle.state::<SeriesLibrary>().inner().clone();
    // 重新打开当前项目是空操作。写入锁已经握在这个进程自己的 `CurrentProject` 里，而 `open_chapter`
    // 是先开新项目、后drop 旧项目，所以同一个项目走一圈只会撞上自己持有的 `project.lock`。
    if handle
        .state::<CurrentProject>()
        .project
        .lock()
        .await
        .as_ref()
        .is_some_and(|project| project.reference == reference)
    {
        return Ok(());
    }
    let opened = library.open(&reference, series.chapter_label(&reference)).await?;
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

/// 关掉当前打开的项目。
///
/// 删除路径也走这里：活动项目在内核里还持有一份打开的场景，不能直接把它从盘上删掉。
pub(crate) async fn close_current_project(handle: &AppHandle<CefRuntime>) -> Result<()> {
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