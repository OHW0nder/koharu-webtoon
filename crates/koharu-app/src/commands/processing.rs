use std::{collections::HashMap, fmt, sync::Arc};

use anyhow::{Context as _, Result, bail};
use koharu_pipeline::{Committer, Progress, RunStatus, StageOutput, StopToken};
use koharu_scene::Snapshot;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager as _, State, ipc::Channel};
use tauri_runtime_cef::CefRuntime;
use tokio::sync::oneshot;
use uuid::Uuid;

use super::{
    ChannelExt as _, Error,
    canvas::CanvasChannel,
    project::{CurrentProject, ProjectLibrary},
    series::SeriesLibrary,
};
use crate::injection;
use koharu_desktop::Desktop;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct JobId(Uuid);

impl JobId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Job {
    pub id: JobId,
    pub state: JobState,
    #[specta(type = f64)]
    pub completed: usize,
    #[specta(type = f64)]
    pub total: usize,
    pub page: Option<koharu_scene::EntityId>,
    pub stage: Option<koharu_pipeline::Stage>,
    pub model: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Finished,
    Failed,
    Stopped,
}

#[derive(Default)]
pub(crate) struct Processing {
    pub(crate) stops: Mutex<HashMap<JobId, StopToken>>,
    pub(crate) jobs: Mutex<HashMap<JobId, Job>>,
    pub(crate) inpainting_mask: Mutex<Option<koharu_pipeline::InpaintingMask>>,
}

impl Processing {
    /// 有没有作业正在跑。
    ///
    /// 判据是 `stops` 而不是 `jobs`：作业登记时插入 `stops`、结束时移除，而 `jobs` 会把终态一直留着，
    /// 直到换项目或关窗口才清。用 `jobs` 判会读到早已结束的历史。
    pub(crate) fn is_running(&self) -> bool {
        !self.stops.lock().is_empty()
    }
}

#[derive(Default)]
pub(crate) struct JobChannel {
    pub(crate) channel: Mutex<Option<Channel<Job>>>,
}

/// 一次已登记的作业。
///
/// **终态随作业一起交出来，而不是让调用方回头查 `Processing::jobs`。** 作业结束时本来就会把自己从
/// 那个 map 里移除（移除后经 `JobChannel` 广播终态），所以 map 里只有运行中的作业；`replace_project`
/// 每次切章还会 `jobs.clear()`。批量要等一章跑完，只能拿这一个一次性的接收端。
pub(crate) struct Started {
    pub(crate) id: JobId,
    terminal: oneshot::Receiver<JobState>,
}

impl Started {
    /// 等到作业跑完，失败与被停都算跑不完。
    ///
    /// 终态的解释收在这里而不是留给调用处：作业自己的收尾决定它算完成、失败还是被停，调用方只需要
    /// 知道「这一章算不算跑完了」。任务被丢弃意味着作业没能走到收尾，那按失败算而不是当成完成。
    pub(crate) async fn terminal(self) -> Result<()> {
        match self
            .terminal
            .await
            .context("the job's task was dropped before it finished")?
        {
            JobState::Finished => Ok(()),
            JobState::Failed => bail!("the processing job failed"),
            JobState::Stopped => bail!("the processing job was stopped"),
            JobState::Running => bail!("the processing job ended without finishing"),
        }
    }
}

/// 这一次运行用哪一部漫画的资料。
pub(crate) enum Subject {
    /// 当前打开的项目。漫画归属要反查才知道；不是任何漫画的章，就只跑用户自己的全局指导。
    CurrentProject,
    /// 调用方已经算好的某章资料。批量在批首扫完整部漫画，逐章切换，走这条。
    ThisChapter(injection::Prepared),
}

/// Runs the pipeline over the open project and returns as soon as the job is registered.
///
/// 漫画资料在这一层算，不在批量那一层：单独打开一章跑和在同一批里跑这一章，必须拿到同一段提示词，
/// 否则「跑批」和「补跑一章」会译出两种结果，而用户看不出区别。
#[tauri::command]
#[specta::specta]
pub(crate) async fn process(
    handle: AppHandle<CefRuntime>,
    scope: koharu_pipeline::Scope,
    operation: koharu_pipeline::Operation,
    processing: State<'_, Processing>,
    job_channel: State<'_, JobChannel>,
) -> std::result::Result<JobId, Error> {
    let started = start_job(
        handle,
        scope,
        operation,
        processing,
        job_channel,
        Subject::CurrentProject,
    )
    .await?;
    Ok(started.id)
}

/// 注册并启动一次作业，跑完后把漫画资料从管线配置里还原。
///
/// 资料必须在作业登记之后才算：算资料要读盘、可能失败，而登记过却没有作业在跑会把这个槽位永久占住。
/// 失败时把槽位还回去，否则这一次点运行就能让后面每一次都撞上「已有作业在跑」。
#[allow(clippy::too_many_arguments)]
pub(crate) async fn start_job(
    handle: AppHandle<CefRuntime>,
    scope: koharu_pipeline::Scope,
    operation: koharu_pipeline::Operation,
    processing: State<'_, Processing>,
    job_channel: State<'_, JobChannel>,
    subject: Subject,
) -> std::result::Result<Started, Error> {
    // 名字与场景必须同一次加锁取出来：名字要拿去反查漫画归属，场景是这一次真正处理的，两者错配就会
    // 把一部漫画的资料算到另一章上。
    let (name, snapshot) = {
        let current = handle.state::<CurrentProject>();
        let project = current.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        (project.name.clone(), project.snapshot())
    };
    let id = JobId::new();
    let stop = StopToken::default();
    {
        let mut stops = processing.stops.lock();
        if !stops.is_empty() {
            return Err(anyhow::anyhow!("another process is already running").into());
        }
        stops.insert(id, stop.clone());
    }
    let prepared = match prepare(&handle, &name, subject).await {
        Ok(prepared) => prepared,
        Err(error) => {
            processing.stops.lock().remove(&id);
            return Err(error.into());
        }
    };
    if let Some(prepared) = &prepared
        && let Err(error) = prepared.apply(&handle)
    {
        processing.stops.lock().remove(&id);
        return Err(error.into());
    }
    let restore = prepared.is_some();
    let job = Job {
        id,
        state: JobState::Running,
        completed: 0,
        total: 0,
        page: None,
        stage: None,
        model: None,
        error: None,
    };
    processing.jobs.lock().insert(id, job.clone());
    job_channel.channel.publish(job);

    let pipeline = handle.state::<koharu_pipeline::Pipeline>().inner().clone();
    let task_handle = handle.clone();
    let inpainting_mask = processing.inpainting_mask.lock().take();
    let (finished, terminal) = oneshot::channel();
    drop(tokio::spawn(async move {
        let progress = Arc::new(Mutex::new((0_usize, 0_usize)));
        let progress_handle = task_handle.clone();
        let mut request = koharu_pipeline::Request {
            operation,
            scope,
            stop: stop.clone(),
            progress: None,
            inpainting_mask,
        };
        request.progress = Some(Arc::new(move |event| {
            let update = match event {
                Progress::Started { pages, stages } => {
                    tracing::info!(
                        target: "koharu_metrics",
                        metric = "pipeline_start",
                        page_count = pages.len(),
                        stage_count = stages.len(),
                    );
                    let mut progress = progress.lock();
                    *progress = (0, pages.len().saturating_mul(stages.len()));
                    Some((0, progress.1, None, None, None))
                }
                Progress::Loading { page, stage, model } => {
                    tracing::info!(
                        target: "koharu_metrics",
                        metric = "stage_loading",
                        stage = %stage,
                        model,
                    );
                    let progress = progress.lock();
                    Some((progress.0, progress.1, Some(page), Some(stage), Some(model)))
                }
                Progress::Finished {
                    page,
                    stage,
                    model,
                    elapsed,
                } => {
                    if stage != koharu_pipeline::Stage::Translation {
                        tracing::info!(
                            target: "koharu_metrics",
                            metric = "model_run",
                            stage = %stage,
                            model,
                            duration_ms = elapsed.as_secs_f64() * 1000.0,
                        );
                    }
                    let mut progress = progress.lock();
                    progress.0 = progress.0.saturating_add(1).min(progress.1);
                    Some((progress.0, progress.1, Some(page), Some(stage), Some(model)))
                }
                Progress::Skipped { page, stage } => {
                    tracing::info!(
                        target: "koharu_metrics",
                        metric = "stage_skip",
                        stage = %stage,
                    );
                    let mut progress = progress.lock();
                    progress.0 = progress.0.saturating_add(1).min(progress.1);
                    Some((progress.0, progress.1, Some(page), Some(stage), None))
                }
                Progress::Running { stage, model, .. } => {
                    tracing::info!(
                        target: "koharu_metrics",
                        metric = "stage_running",
                        stage = %stage,
                        model,
                    );
                    None
                }
            };
            if let Some((completed, total, page, stage, model)) = update {
                let job = {
                    let processing = progress_handle.state::<Processing>();
                    let mut jobs = processing.jobs.lock();
                    jobs.get_mut(&id).map(|job| {
                        job.completed = completed;
                        job.total = total;
                        job.page = page;
                        job.stage = stage;
                        job.model = model;
                        job.clone()
                    })
                };
                if let Some(job) = job {
                    progress_handle.state::<JobChannel>().channel.publish(job);
                }
            }
        }));

        struct PipelineCommitter {
            handle: AppHandle<CefRuntime>,
        }

        #[async_trait::async_trait]
        impl Committer for PipelineCommitter {
            async fn commit(&mut self, output: StageOutput) -> Result<Snapshot> {
                let (commit, page) = {
                    let projects = self.handle.state::<CurrentProject>();
                    let mut projects = projects.project.lock().await;
                    let project = projects.as_mut().context("no project is open")?;
                    let Some(commit) = project.commit_rebased(output.patch).await? else {
                        return Ok(project.snapshot());
                    };
                    project.record_commit(&commit);
                    let page = project.active_page();
                    (commit, page)
                };
                let snapshot = commit.snapshot.clone();
                let desktop = self.handle.state::<Desktop>();
                desktop.synchronize(&commit.snapshot, page, &commit).await?;
                let canvas = desktop.canvas_state();
                self.handle.state::<CanvasChannel>().channel.publish(canvas);
                Ok(snapshot)
            }
        }

        let mut committer = PipelineCommitter {
            handle: task_handle.clone(),
        };
        let result = pipeline.execute(snapshot, request, &mut committer).await;
        // 还原在成功与失败之后都要做，而且要早于作业转入终态：句柄是全局的，残留的漫画资料会漏进设置页，
        // 也会被下一次运行当成用户自己的设置读走。
        if restore {
            if let Err(error) = injection::restore(&task_handle) {
                tracing::error!(%error, "failed to restore the pipeline configuration");
            }
        }
        let (stopped, error) = match result {
            Ok(report) => (report.status == RunStatus::Stopped, None),
            Err(error) => {
                tracing::error!(stage = ?error.stage, %error, "processing failed");
                (false, Some(format!("{error:#}")))
            }
        };
        let state = if stopped {
            JobState::Stopped
        } else if error.is_some() {
            JobState::Failed
        } else {
            JobState::Finished
        };
        tracing::info!(
            target: "koharu_metrics",
            metric = "pipeline_result",
            outcome = if stopped {
                "stopped"
            } else if error.is_some() {
                "failed"
            } else {
                "completed"
            },
        );
        task_handle.state::<Processing>().stops.lock().remove(&id);
        let job = task_handle
            .state::<Processing>()
            .jobs
            .lock()
            .remove(&id)
            .map(|mut job| {
                job.state = state;
                job.error = error;
                job
            });
        if let Some(job) = job {
            task_handle.state::<JobChannel>().channel.publish(job);
        }
        // 广播是给界面的，这条是给批量的。两者都要有：`JobChannel` 只有一条，而它上面正挂着这一次
        // 作业的进度，批量必须另拿一个终态而不是和界面抢。
        let _ = finished.send(state);
    }));
    Ok(Started { id, terminal })
}

/// 这一次运行该注入什么。
///
/// 批量已经把整部漫画扫完并且知道自己在跑哪一章，所以直接用调用方给的。单章运行只有项目名，先反查它
/// 属于哪部漫画；查不到就只跑用户自己的全局指导，那条路径没有任何漫画资料。
async fn prepare(
    handle: &AppHandle<CefRuntime>,
    project: &str,
    subject: Subject,
) -> Result<Option<injection::Prepared>> {
    let prepared = match subject {
        Subject::ThisChapter(prepared) => Some(prepared),
        Subject::CurrentProject => chapter_subject(handle, project).await?,
    };
    Ok(prepared)
}

/// 单章运行的资料：反查这个项目属于哪部漫画，是就算出这一次的。查不到就只跑用户自己的全局指导。
async fn chapter_subject(
    handle: &AppHandle<CefRuntime>,
    project: &str,
) -> Result<Option<injection::Prepared>> {
    let library = handle.state::<SeriesLibrary>().inner().clone();
    let Some(series) = library.owning_series(project)? else {
        return Ok(None);
    };
    // 与跑批同一趟预扫描：整部漫画的原文决定术语命中哪一批词条，所以命中结果不随运行方式变化。
    let projects = handle.state::<ProjectLibrary>().inner().clone();
    let assets = injection::Assets::collect(&library, &series, &projects).await?;
    let baseline = koharu_pipeline::PipelineConfig::load()?.read()?.clone();
    injection::Prepared::for_chapter(&assets, project, &baseline).map(Some)
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "pipeline_stop",
    skip_all,
    fields(state = "requested")
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn stop_job(
    job: JobId,
    processing: State<'_, Processing>,
) -> std::result::Result<(), Error> {
    let stops = processing.stops.lock();
    let stop = stops
        .get(&job)
        .with_context(|| format!("job {job} is not running"))?;
    stop.stop();
    Ok(())
}
