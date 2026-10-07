//! 生肉来源：绑定站点、检查更新、把缺的章节下载下来并走正常导入。
//!
//! 这一层拥有「怎么和站点打交道」的全部决定：一次下几页、章与章之间隔多久、临时目录放在哪、
//! 缺了哪几章。它不碰站点协议本身（那是 `koharu-source` 的事），也不碰导入怎么做
//! （那是 `ingest_chapter` 的事），只负责把两者接起来。
//!
//! **原图不留盘。** 一章的页先落进漫画目录下的临时目录，交给 `ingest_chapter` 走完与手动导入
//! 完全相同的那条预处理，然后整个临时目录删掉。koharu 不在磁盘上保留生肉，图片进内容寻址的
//! blob，所以留下来只是双份存储；而判据是索引里的章名而不是目录，因此这一章要么完整进库、
//! 要么什么都没留下，不需要「下到一半的目录看起来像已完成」这类防护。

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use futures::{StreamExt as _, TryStreamExt as _, stream};
use koharu_pipeline::StopToken;
use koharu_source::omegascans::{
    self, ChapterPages, chapter_directory_name, image_extension, page_file_name,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager as _, State, ipc::Channel};
use tauri_runtime_cef::CefRuntime;
use tempfile::TempDir;

use super::{
    ChannelExt as _, Error,
    processing::Processing,
    project::ProjectLibrary,
    reject_import_while_processing,
    series::{ChapterKind, SeriesLibrary, SeriesSite, SeriesSource, ingest_chapter},
};

/// 同一章内并发的页数。
///
/// 刻意低于 `koharu-runtime` 那套 8..=32 的连接数：那一套按「本机带宽能吃满」定的，
/// 而这里对着的是一个人的站，不是 CDN。
const PAGE_CONCURRENCY: usize = 4;

/// 单页的尝试次数，含第一次。
const PAGE_ATTEMPTS: u32 = 3;

/// 章与章之间的间隔。给站点一点喘息，也让「下载中」不至于把整站压满。
const CHAPTER_DELAY: Duration = Duration::from_millis(500);

/// 临时下载目录的前缀。
///
/// 放在漫画目录内而不是系统临时目录：图片要先落一次盘再进 blob，同盘比跨盘快，而崩溃之后
/// 残留的目录也能在下次清扫掉。前导点是为了让它一眼看出不是一章。
const STAGING_PREFIX: &str = ".fetch-";

/// 站点上一篇作品的一章。
///
/// 同时是「检查更新」的返回项与「开始下载」的入参：前端原样回传后端刚给的东西，后端因此
/// 不必为了拿 slug 再把整个章节列表拉一遍。
#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct SourceChapter {
    /// 归一化后的章名，也就是本地将用的 `SeriesChapter.title`。
    pub name: String,
    /// 阅读页地址的最后一段。
    pub slug: String,
}

/// 检查更新的结果。
#[derive(Clone, Debug, Serialize, Type)]
pub struct SourceCheck {
    /// 站点上的作品名，用来核对绑对了没有。
    pub title: String,
    /// 下载时会用的源形态。界面照它显示，用户才知道条漫漫画不会被当成页漫。
    pub kind: ChapterKind,
    /// 本地还没有的章节，按章号升序。
    pub missing: Vec<SourceChapter>,
}

/// 一次下载任务的进度。
#[derive(Clone, Debug, Serialize, Type)]
pub struct SourceFetch {
    pub id: u32,
    pub series: String,
    pub chapter: String,
    /// 从 1 起算，让界面不必自己加。
    pub chapter_index: u32,
    pub chapter_total: u32,
    pub page_index: u32,
    pub page_total: u32,
    pub state: SourceFetchState,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourceFetchState {
    Running,
    /// 全部选中章节都已进库。
    Finished,
    /// 中途失败。已经进库的章节仍然留在库里。
    Failed,
    /// 用户叫停。
    Stopped,
}

#[derive(Default)]
pub(crate) struct SourceFetchChannel {
    pub(crate) channel: Mutex<Option<Channel<SourceFetch>>>,
}

/// 正在跑的下载任务。
///
/// 只登记运行中的：任务结束时自己从表里摘掉，所以「有没有在跑」判这张表就行——
/// 和 `Processing::stops` 一个道理，只是这份表按漫画分槽，而处理作业全局只有一个槽。
#[derive(Default)]
pub(crate) struct SourceFetches {
    entries: Mutex<HashMap<u32, Running>>,
    next: AtomicU32,
}

struct Running {
    series: String,
    stop: StopToken,
}

impl SourceFetches {
    /// 登记一个新任务。
    ///
    /// 同一漫画同时只允许一个：两次运行会各自去下同一章，然后撞上同名目录，而失败那一次的
    /// 回滚会把成功那一次刚写好的索引一起抹掉。
    fn register(&self, series: &str) -> Result<(u32, StopToken)> {
        let mut entries = self.entries.lock();
        if entries.values().any(|entry| entry.series == series) {
            bail!("a fetch for this series is already running");
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let stop = StopToken::default();
        entries.insert(
            id,
            Running {
                series: series.to_owned(),
                stop: stop.clone(),
            },
        );
        Ok((id, stop))
    }

    fn finish(&self, id: u32) {
        self.entries.lock().remove(&id);
    }

    /// 叫停在跑的那个任务。任务号已经不在表里时什么都不做——它已经结束了。
    fn stop(&self, id: u32) {
        if let Some(entry) = self.entries.lock().get(&id) {
            entry.stop.stop();
        }
    }
}

/// 一次下载要跑的东西。
struct Plan {
    /// 任务号，也是进度事件的 id。
    task: u32,
    /// 漫画目录名。
    id: String,
    source: SeriesSource,
    /// 这一批章的源形态，在开始之前就定好，不逐章问。
    kind: ChapterKind,
    chapters: Vec<SourceChapter>,
}

/// 把这部漫画的生肉来源设成用户给的那个地址。
///
/// 地址先解析成 slug，再向站点核对一次才落盘：当 slug 用会静默拉到别的作品上，而绑定错了要等到
/// 下载完才发现。传 `None` 解绑。
///
/// 这里假定站点只有一个所以不问「哪个站点」；真要加第二个站点时，这里要显式收一个站点参数，
/// 而不是从域名去猜——猜错的表现和现在传错 slug 一样。
#[tracing::instrument(
    target = "koharu_metrics",
    name = "series_source_bound",
    skip_all,
    fields(origin = "user", series = %id),
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn set_series_source(
    id: String,
    address: Option<String>,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<super::series::Series, Error> {
    let mut series = library.read(&id)?;
    series.source = match address {
        Some(address) => {
            let slug = omegascans::parse_series_url(&address)?;
            // 核对顺带把作品名取回来，界面能显示绑的到底是哪一部。
            omegascans::Client::new()?.series(slug).await?;
            Some(SeriesSource {
                site: SeriesSite::OmegaScans,
                slug: slug.to_owned(),
            })
        }
        None => None,
    };
    library.write(&series)?;
    Ok(series)
}

/// 站点上有哪些章是本地还没有的。
#[tracing::instrument(
    target = "koharu_metrics",
    name = "series_source_checked",
    skip_all,
    fields(origin = "user", series = %id),
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn check_series_updates(
    id: String,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<SourceCheck, Error> {
    let series = library.read(&id)?;
    let source = series
        .source
        .clone()
        .context("this series has no source bound")?;
    let client = omegascans::Client::new()?;
    let remote = client.series(&source.slug).await?;
    let chapters = client.chapters(remote.id).await?;

    // 判据是章名。`SeriesChapter.title` 本来就等于来源目录名，`ingest_chapter` 也已经拿它查重，
    // 所以既不需要额外台账，也不会和手动导入记下的同一章重复计算。
    let local: HashSet<&str> = series
        .chapters
        .iter()
        .map(|chapter| chapter.title.as_str())
        .collect();
    let missing = chapters
        .iter()
        .map(|chapter| SourceChapter {
            name: chapter_directory_name(&chapter.name),
            slug: chapter.slug.clone(),
        })
        .filter(|chapter| !local.contains(chapter.name.as_str()))
        .collect();

    Ok(SourceCheck {
        title: remote.title,
        kind: inherited_kind(&series)?,
        missing,
    })
}

/// 这一批下载到的章按哪种源形态处理。
///
/// 形态跟着漫画走，而不是逐章问用户：条漫的下载也必须产出条漫章项目，否则切页被跳过，
/// 竖长图会在画布上糊成一张。取 seq 最大的那一章——数据模型允许同一部漫画混形态，所以
/// 「继承」得有确定规则，而最近加入的那一章是用户最后一次表达的意图。
///
/// 检查与下载都要这个答案，所以规则只在这里写一遍：前端那份要是自己算，两边迟早会不一致，
/// 而不一致的表现是界面说「条漫」而下载按页漫跑完一整章。
fn inherited_kind(series: &super::series::Series) -> Result<ChapterKind> {
    series
        .chapters
        .iter()
        .max_by_key(|chapter| chapter.seq)
        .map(|chapter| chapter.kind)
        .context("this series has no chapters yet, so there is no source form to inherit")
}

/// 把选中的章节下载下来，走正常导入进库。
#[tracing::instrument(
    target = "koharu_metrics",
    name = "series_source_fetched",
    skip_all,
    fields(origin = "user", series = %id, chapters = chapters.len()),
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn start_fetch(
    id: String,
    chapters: Vec<SourceChapter>,
    app: AppHandle<CefRuntime>,
    library: State<'_, SeriesLibrary>,
    projects: State<'_, ProjectLibrary>,
    processing: State<'_, Processing>,
    fetches: State<'_, SourceFetches>,
) -> std::result::Result<u32, Error> {
    reject_import_while_processing(&processing)?;
    if chapters.is_empty() {
        return Err(anyhow::anyhow!("no chapters were selected").into());
    }
    let series = library.read(&id)?;
    let source = series
        .source
        .clone()
        .context("this series has no source bound")?;
    let kind = inherited_kind(&series)?;

    let (task, stop) = fetches.register(&id)?;
    let plan = Plan {
        task,
        id: id.clone(),
        source,
        kind,
        chapters,
    };

    // 任务要在命令返回之后继续，所以该带的都带走，不留 `State` 的借用。
    let library = library.inner().clone();
    let projects = projects.inner().clone();
    tauri::async_runtime::spawn(async move {
        let outcome = run(&app, &library, &projects, &plan, &stop).await;
        // 无论成败都要摘掉登记，否则「有没有在跑」会永远为真，这部漫画再也起不了新任务。
        app.state::<SourceFetches>().finish(plan.task);
        report_terminal(&app, &plan, &stop, outcome);
    });
    Ok(task)
}

/// 请求停掉一次下载。
///
/// 在下一个页面边界生效。当前这一章因此不会进库，它的临时目录也一起丢掉；而已经进库的章
/// 留在库里。之所以不打断正在写的那一页：那样会留下一个页数不齐的章项目，那比缺一章难发现得多。
#[tauri::command]
#[specta::specta]
pub(crate) fn cancel_fetch(
    id: u32,
    fetches: State<'_, SourceFetches>,
) -> std::result::Result<(), Error> {
    fetches.stop(id);
    Ok(())
}

/// 逐章下载并导入。
async fn run(
    app: &AppHandle<CefRuntime>,
    library: &SeriesLibrary,
    projects: &ProjectLibrary,
    plan: &Plan,
    stop: &StopToken,
) -> Result<()> {
    let client = omegascans::Client::new()?;
    let series_directory = library.path(&plan.id);
    // 登记时已经挡住同一部漫画的第二个任务，所以这里看见的必然是上次崩溃留下的。
    sweep_stale_staging(&series_directory)?;

    for (index, chapter) in plan.chapters.iter().enumerate() {
        if stop.stopped() {
            bail!("the fetch was stopped before {}", chapter.name);
        }

        // 临时父目录里一次只放一章：`collect_importable` 是递归的，平级两个章目录会被一起
        // 吞进同一章。而目录名就是章名——`ingest_chapter` 靠它定 `title`，临时目录不能用随机名。
        let staging = staging_directory(&series_directory)?;
        let pages_directory = staging.path().join(&chapter.name);
        fs::create_dir_all(&pages_directory)
            .with_context(|| format!("failed to create {}", pages_directory.display()))?;

        let pages = client
            .chapter_pages(&plan.source.slug, &chapter.slug)
            .await
            .with_context(|| format!("{} could not be listed", chapter.name))?;
        if pages.invalid > 0 {
            tracing::warn!(
                series = %plan.id,
                chapter = %chapter.name,
                invalid = pages.invalid,
                "the site listed image addresses that are not usable"
            );
        }
        if pages.urls.is_empty() {
            // 站点认得这一章却不给图：通常是付费或尚未解锁。跳过，后面的章照跑。
            tracing::info!(series = %plan.id, chapter = %chapter.name, "the chapter lists no pages yet");
            continue;
        }

        download_pages(&client, &pages, &pages_directory, app, plan, index, chapter, stop).await?;

        // 形态与广告带都沿用这部漫画自己的设置，与手动导入走的是同一条路径。
        ingest_chapter(library, projects, &plan.id, &pages_directory, plan.kind, None)
            .await
            .with_context(|| format!("{} could not be imported", chapter.name))?;

        // 到这里像素已经在 blob 里，临时目录（含 `staging` 的 drop）不再需要任何东西。
        drop(staging);

        if index + 1 < plan.chapters.len() && !stop.stopped() {
            tokio::time::sleep(CHAPTER_DELAY).await;
        }
    }
    Ok(())
}

/// 下载一章的所有页。
///
/// 任何一页失败就整章作废：少了一页的章项目比没有这一章更难发现，而重新下一章的代价只是
/// 下一次检查时它还在缺失清单里。
async fn download_pages(
    client: &omegascans::Client,
    pages: &ChapterPages,
    directory: &Path,
    app: &AppHandle<CefRuntime>,
    plan: &Plan,
    chapter_index: usize,
    chapter: &SourceChapter,
    stop: &StopToken,
) -> Result<()> {
    let total = pages.urls.len();
    let fetched = AtomicUsize::new(0);

    // 流必须拥有地址而不是借用：借用会让下面那个闭包对生命周期泛化，
    // 而 `spawn` 要求整个 future 是 `'static`，泛型化的闭包过不了那一关。
    stream::iter(pages.urls.iter().cloned().enumerate())
        .map(|(page_index, url)| {
            let client = client.clone();
            let directory = directory.to_owned();
            async move {
                if stop.stopped() {
                    bail!("the fetch was stopped while downloading {}", chapter.name);
                }
                let destination =
                    directory.join(page_file_name(page_index, total, image_extension(&url)));
                // 站点偶发的连接重置与 5xx 在漫画站上是常态，重试比让整章失败划算。
                let mut last = None;
                for attempt in 0..PAGE_ATTEMPTS {
                    match client.image(&url).await {
                        Ok(bytes) => {
                            tokio::fs::write(&destination, bytes)
                                .await
                                .with_context(|| format!("failed to write {}", destination.display()))?;
                            return Ok(());
                        }
                        Err(error) => {
                            last = Some(error);
                            if attempt + 1 < PAGE_ATTEMPTS {
                                tokio::time::sleep(Duration::from_millis(
                                    500 * u64::from(attempt) + 500,
                                ))
                                .await;
                            }
                        }
                    }
                }
                Err(anyhow::anyhow!(
                    "{} could not be fetched: {}",
                    destination.display(),
                    last.map(|error| format!("{error:#}")).unwrap_or_default()
                ))
            }
        })
        .buffer_unordered(PAGE_CONCURRENCY)
        // 闭包收到的是成功的那一半，失败由组合器自己短路传播，所以这里只数成功的页。
        .try_for_each(|_| async {
            let done = fetched.fetch_add(1, Ordering::Relaxed) + 1;
            report_progress(app, plan, chapter_index, &chapter.name, done, total);
            Ok::<(), anyhow::Error>(())
        })
        .await
}

/// 在漫画目录下开一个临时目录。
fn staging_directory(series_directory: &Path) -> Result<TempDir> {
    tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(series_directory)
        .context("the series folder cannot hold a temporary download directory")
}

/// 清掉上次崩溃留下的临时目录。
///
/// 登记时已经保证同一漫画没有第二个任务在跑，所以这里看见的必然是残留。范围严格限定在带前缀
/// 的目录上——这是本功能唯一一处会在用户的内容目录里删东西。
fn sweep_stale_staging(series_directory: &Path) -> Result<()> {
    let entries = fs::read_dir(series_directory)
        .with_context(|| format!("failed to read {}", series_directory.display()))?;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type().is_ok_and(|kind| kind.is_dir())
            || !entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(STAGING_PREFIX))
        {
            continue;
        }
        fs::remove_dir_all(entry.path()).with_context(|| {
            format!(
                "failed to remove the stale download directory {}",
                entry.path().display()
            )
        })?;
    }
    Ok(())
}

/// 报一次进度。错误文本只有终态才带，所以这里不放。
fn report_progress(
    app: &AppHandle<CefRuntime>,
    plan: &Plan,
    chapter_index: usize,
    chapter: &str,
    page_index: usize,
    page_total: usize,
) {
    app.state::<SourceFetchChannel>()
        .channel
        .publish(SourceFetch {
            id: plan.task,
            series: plan.id.clone(),
            chapter: chapter.to_owned(),
            chapter_index: chapter_index as u32 + 1,
            chapter_total: plan.chapters.len() as u32,
            page_index: page_index as u32,
            page_total: page_total as u32,
            state: SourceFetchState::Running,
            error: None,
        });
}

/// 报终态。
///
/// 用户叫停与真失败要分开：前者是用户自己的选择，界面上不该弹成红色错误。
fn report_terminal(
    app: &AppHandle<CefRuntime>,
    plan: &Plan,
    stop: &StopToken,
    outcome: Result<()>,
) {
    let (state, error) = match outcome {
        Ok(()) => (SourceFetchState::Finished, None),
        Err(_) if stop.stopped() => (SourceFetchState::Stopped, None),
        Err(error) => {
            tracing::warn!(series = %plan.id, %error, "the fetch did not finish");
            (SourceFetchState::Failed, Some(format!("{error:#}")))
        }
    };
    app.state::<SourceFetchChannel>()
        .channel
        .publish(SourceFetch {
            id: plan.task,
            series: plan.id.clone(),
            chapter: String::new(),
            chapter_index: plan.chapters.len() as u32,
            chapter_total: plan.chapters.len() as u32,
            page_index: 0,
            page_total: 0,
            state,
            error,
        });
}