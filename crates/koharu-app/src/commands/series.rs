//! 漫画层：一部漫画作为一组章项目的索引，再加上漫画自己的数据。
//!
//! 内核只认项目（`.khrproj`），漫画不是内核概念，而是这一层维护的索引。它与章项目解耦：
//! 删掉一章不影响漫画定义，删掉漫画也不会去动章项目，两边的生命周期各自独立。
//!
//! 索引放在漫画目录里，因此它跟着漫画一起被移动、备份和删除。章在索引里只记项目名，因为
//! 项目名是内核唯一稳定的公开标识；章项目在内核眼中仍然是普通项目，能被单独打开、单独
//! 导出、单独跑流水线。
//!
//! 漫画目录就是项目根目录下一个不带 `.khrproj` 后缀的文件夹，所以内核的项目枚举会自然跳过
//! 它们，两套目录并存不需要改内核。

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager as _, State, WebviewWindow};
use tauri_runtime_cef::CefRuntime;

use super::{
    Error,
    import::{self, Format, Slicing},
    lifecycle::replace_project,
    output::{ExportFormat, export_snapshot},
    processing::{JobChannel, JobId, JobState, Processing, process},
    project::{CurrentProject, ProjectLibrary},
    reject_import_while_processing,
};

/// 索引文件名。
const INDEX: &str = "series.json";

/// 一部漫画。
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct Series {
    /// 目录名，同时是这部漫画的标识。
    ///
    /// 它由所在位置决定，所以读取时以目录名为准覆盖文件里的值；写进文件只是为了在目录被
    /// 改名之后还能看出原委。
    pub id: String,
    pub title: String,
    /// 连载为 `true`，单行为 `false`。只影响界面呈现，不改变任何处理逻辑。
    pub serial: bool,
    /// 封面文件名。`None` 表示用户没有指定，界面渲染占位图。
    pub cover: Option<String>,
    /// 下载器的输出目录；扫描新章时用它找出还没登记的目录。
    pub source_root: Option<PathBuf>,
    pub chapters: Vec<SeriesChapter>,
    pub settings: SeriesSettings,
}

/// 漫画级的翻译资料与配置。
///
/// 归属漫画而不是章：资料的生命周期跟着整部作品走，而章项目会被删除重建。现在刻意不设
/// 字段，术语表与角色卡是它的第一批成员。
#[derive(Clone, Debug, Default, Serialize, Deserialize, Type)]
pub struct SeriesSettings {}

/// 漫画里的一章。
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SeriesChapter {
    pub seq: u32,
    pub title: String,
    /// 章项目名，对应 `<root>/<project>.khrproj`。
    pub project: String,
    /// 相对 `source_root` 的源目录名；`None` 表示这一章就是 `source_root` 本身，即单行本。
    pub source: Option<String>,
    /// 这一章的源形态，决定导入时是否切页。
    pub kind: ChapterKind,
    /// 编排状态，与章项目内部的处理状态分开。
    pub status: ChapterStatus,
}

/// 一章的源形态。导入时由用户选定，不靠自动判定：页漫里存在高幅面单页，条漫的形态也可能
/// 与之接近，判错就是整章被切碎或完全没切。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChapterKind {
    /// 页漫：多张普通页，每张一页，不切。
    Manga,
    /// 条漫：一张纵向长图，按可读高度切页。
    Webtoon,
}

/// 一章的编排状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChapterStatus {
    /// 已在索引中登记，章项目尚未建立。
    Pending,
    /// 章项目已建立，可以进入处理流程。
    Ready,
    Done,
}

/// 首页漫画柜需要的一条漫画。
#[derive(Clone, Debug, Serialize, Type)]
pub struct SeriesSummary {
    pub id: String,
    pub title: String,
    pub serial: bool,
    /// 封面文件名，`None` 表示用占位图。
    pub cover: Option<String>,
    pub chapters: u32,
    pub done: u32,
}

/// 一个源目录的形状。
#[derive(Clone, Debug)]
pub(crate) enum SourceShape {
    /// 有子目录，每个子目录是一章。
    Serial(Vec<PathBuf>),
    /// 没有可导入的子目录，目录本身是一章。
    OneShot,
}

/// 漫画目录的发现与读写。
#[derive(Clone)]
pub(crate) struct SeriesLibrary {
    root: PathBuf,
}

impl SeriesLibrary {
    /// 根目录取自项目库而不是自己再算一遍：位置只有一处决定，官方若移动它也不会漂移。
    pub(crate) fn new(library: &ProjectLibrary) -> Self {
        Self {
            root: library.root().to_owned(),
        }
    }

    /// 列出全部漫画，按目录最近修改时间倒序，与项目列表的排序保持一致。
    pub(crate) fn list(&self) -> Result<Vec<SeriesSummary>> {
        let mut entries = fs::read_dir(&self.root)
            .with_context(|| format!("failed to read {}", self.root.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| {
                let path = entry.path();
                let id = path.file_stem()?.to_str()?.to_owned();
                Some((path, id))
            })
            .filter(|(path, _)| path.join(INDEX).is_file())
            .filter_map(|(path, id)| {
                let touched = fs::metadata(path).ok()?.modified().ok()?;
                let series = self.read(&id).ok()?;
                Some((touched, series))
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by(|(left_touched, left), (right_touched, right)| {
            right_touched
                .cmp(left_touched)
                .then_with(|| left.title.to_lowercase().cmp(&right.title.to_lowercase()))
        });
        Ok(entries.into_iter().map(|(_, series)| series.into()).collect())
    }

    /// 全部漫画已认领的章项目名。
    ///
    /// 章项目与漫画共用项目根目录、同一套 `.khrproj` 实体，分组与否只取决于索引里有没有它，
    /// 所以"未被认领"就是未分组项目的准确定义。
    ///
    /// 单部漫画的索引缺失或损坏时跳过它，而不是让整个项目列表查不出来：它仍然出现在漫画柜
    /// 里，只是这一轮不参与过滤。
    pub(crate) fn claimed_projects(&self) -> Result<BTreeSet<String>> {
        let mut claimed = BTreeSet::new();
        let entries =
            fs::read_dir(&self.root).with_context(|| format!("failed to read {}", self.root.display()))?;
        for entry in entries.filter_map(|entry| entry.ok()) {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Ok(contents) = fs::read_to_string(entry.path().join(INDEX)) else {
                continue;
            };
            let Ok(series) = serde_json::from_str::<Series>(&contents) else {
                continue;
            };
            claimed.extend(series.chapters.into_iter().map(|chapter| chapter.project));
        }
        Ok(claimed)
    }

    /// 读一部漫画，章按序号排序。
    pub(crate) fn read(&self, id: &str) -> Result<Series> {
        let path = self.path(id);
        let contents = fs::read_to_string(path.join(INDEX))
            .with_context(|| format!("failed to read the index of series {id:?}"))?;
        let mut series: Series = serde_json::from_str(&contents)
            .with_context(|| format!("failed to parse the index of series {id:?}"))?;
        // 目录名才是标识，文件里的值可能落后于一次手工改名。
        series.id = id.to_owned();
        series.chapters.sort_by_key(|chapter| chapter.seq);
        Ok(series)
    }

    /// 写回索引。先写临时文件再改名，中途失败不会留下半份索引。
    pub(crate) fn write(&self, series: &Series) -> Result<()> {
        let directory = self.path(&series.id);
        fs::create_dir_all(&directory)
            .with_context(|| format!("failed to create {}", directory.display()))?;
        let contents = serde_json::to_string_pretty(series)
            .context("failed to encode the series index")?;
        let index = directory.join(INDEX);
        let temporary = directory.join("series.json.tmp");
        fs::write(&temporary, contents)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        fs::rename(&temporary, &index)
            .with_context(|| format!("failed to publish {}", index.display()))?;
        Ok(())
    }

    /// 为一部新漫画分配一个没有被占用的目录名。
    pub(crate) fn reserve_id(&self, title: &str) -> Result<String> {
        directory_name(title, |candidate| self.path(candidate).exists())
    }

    fn path(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }
}

impl From<Series> for SeriesSummary {
    fn from(series: Series) -> Self {
        Self {
            chapters: series.chapters.len() as u32,
            done: series
                .chapters
                .iter()
                .filter(|chapter| chapter.status == ChapterStatus::Done)
                .count() as u32,
            id: series.id,
            title: series.title,
            serial: series.serial,
            cover: series.cover,
        }
    }
}

/// 只看一层子目录来判别连载与单行本。
///
/// 判据是"下载器有没有按章分好目录"，而不是递归深度：一个塞满图片的目录和一个已经按章分好
/// 的目录，区别就在这一层。只看一层同时避免了把章内用于分页的子目录误判成新的一章。
pub(crate) fn classify_source(source: &Path) -> Result<SourceShape> {
    if !source.is_dir() {
        bail!("{} is not a directory", source.display());
    }
    if contains_importable(source) {
        return Ok(SourceShape::OneShot);
    }
    let mut children = fs::read_dir(source)
        .with_context(|| format!("failed to read {}", source.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| contains_importable(path))
        .collect::<Vec<_>>();
    children.sort();
    if children.is_empty() {
        bail!(
            "{} holds no images, archives or PDFs, at either level",
            source.display()
        );
    }
    Ok(SourceShape::Serial(children))
}

/// 目录里是否有内核能导入的内容。
fn contains_importable(directory: &Path) -> bool {
    fs::read_dir(directory).is_ok_and(|entries| {
        entries.filter_map(|entry| entry.ok()).any(|entry| {
            entry.file_type().is_ok_and(|kind| kind.is_file())
                && entry
                    .path()
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.parse::<Format>().is_ok())
        })
    })
}

/// 把任意标题变成能安全用在目录名与项目名里的形式。
///
/// 目录名与项目名各自有一套非法字符规则，规则由内核定义、这里无从查询，所以只替换掉两边都
/// 不接受的那几个字符，其余原样保留。结尾的点和空格会被去掉，因为项目名不允许以它们结尾。
fn sanitize(name: &str) -> String {
    name.trim()
        .chars()
        .map(|character| {
            if matches!(
                character,
                '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
            ) {
                '-'
            } else {
                character
            }
        })
        .collect::<String>()
        .trim_matches(|character: char| character == '.' || character == ' ')
        .to_owned()
}

/// 把标题变成一个能安全用作目录名的标识，重名时加数字后缀。
fn directory_name(title: &str, mut taken: impl FnMut(&str) -> bool) -> Result<String> {
    let mut base = sanitize(title);
    if base.is_empty() {
        base = "series".to_owned();
    }
    if !taken(&base) {
        return Ok(base);
    }
    for suffix in 2..u32::MAX {
        let candidate = format!("{base} {suffix}");
        if !taken(&candidate) {
            return Ok(candidate);
        }
    }
    bail!("could not find an unused directory name for {title:?}")
}

/// 漫画柜里的全部漫画。
#[tauri::command]
#[specta::specta]
pub(crate) fn list_series(
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<Vec<SeriesSummary>, Error> {
    Ok(library.list()?)
}

/// One series with its chapters, which is what the chapter list needs.
#[tauri::command]
#[specta::specta]
pub(crate) fn get_series(
    id: String,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<Series, Error> {
    Ok(library.read(&id)?)
}

/// A chapter directory the downloader produced that the series has not claimed yet.
#[derive(Clone, Debug, Serialize, Type)]
pub struct CandidateChapter {
    pub name: String,
    /// The number this chapter would take.
    pub seq: u32,
    /// How many importable files the directory holds. One long image reads very differently from
    /// a page folder, so this is what the user decides the chapter kind on.
    pub files: u32,
}

/// Lists the chapter directories under the series' source folder that are not registered yet.
///
/// Discovery is separate from import on purpose: which kind a chapter is has to be chosen per
/// chapter, and a wrong cut is expensive to undo.
#[tauri::command]
#[specta::specta]
pub(crate) fn scan_series_source(
    id: String,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<Vec<CandidateChapter>, Error> {
    let series = library.read(&id)?;
    let Some(root) = series.source_root.as_ref() else {
        return Ok(Vec::new());
    };
    let known = series
        .chapters
        .iter()
        .filter_map(|chapter| chapter.source.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    let mut next = series.chapters.len() as u32 + 1;

    let mut children = fs::read_dir(root)
        .with_context(|| format!("failed to read {}", root.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| !known.contains(name))
        })
        .collect::<Vec<_>>();
    children.sort();

    Ok(children
        .into_iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.to_owned();
            let files = import::collect_importable(&path).ok()?.len() as u32;
            if files == 0 {
                return None;
            }
            let candidate = CandidateChapter {
                name,
                seq: next,
                files,
            };
            next += 1;
            Some(candidate)
        })
        .collect())
}

/// Imports one chapter of an existing series from its source folder.
#[tauri::command]
#[specta::specta]
pub(crate) async fn import_series_chapter(
    id: String,
    name: String,
    kind: ChapterKind,
    projects: State<'_, ProjectLibrary>,
    library: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<Series, Error> {
    reject_import_while_processing(&processing)?;
    let projects = projects.inner().clone();
    let mut series = library.read(&id)?;
    if series.chapters.iter().any(|chapter| chapter.source.as_deref() == Some(&name)) {
        return Err(anyhow::anyhow!("{name} is already part of this series").into());
    }
    let Some(root) = series.source_root.clone() else {
        return Err(anyhow::anyhow!("this series has no source folder to import from").into());
    };

    let seq = series
        .chapters
        .last()
        .map_or(1, |chapter| chapter.seq + 1);
    let stem = sanitize(&series.title);
    let stem = if stem.is_empty() { "series" } else { &stem };
    let directory = root.join(&name);
    let files = import::collect_importable(&directory)?;
    if files.is_empty() {
        return Err(anyhow::anyhow!("{name} holds no importable pages").into());
    }
    let imported = tokio_rayon::spawn(move || match kind {
        ChapterKind::Manga => import::import(files),
        ChapterKind::Webtoon => import::import_webtoon(files, Slicing::Auto),
    })
    .await?;
    let project_name = format!("{stem} Ch{seq}");
    let mut project = projects.create(&project_name).await?;
    import::apply(&mut project, imported).await?;

    series.chapters.push(SeriesChapter {
        seq,
        title: name.clone(),
        project: project_name,
        source: Some(name),
        kind,
        status: ChapterStatus::Ready,
    });
    library.write(&series)?;
    Ok(series)
}

/// 导入一个文件夹作为一部漫画。
///
/// 有子目录就是连载，每个子目录一章；没有就是单行本，目录本身是一章，章名跟随文件夹名。
/// 两种形态在界面上共用同一套结构，单行本只是永远只有一章，所以界面不必为它开特例分支。
///
/// 每一章都建立成一个独立项目，因此导入多章和导入一章走的是同一条提交路径，也同样受"处理
/// 任务运行中不可导入"的约束。索引先落盘再建项目：中途失败时已登记的章还在，重试不会留下
/// 一部没有记录的漫画。
#[tauri::command]
#[specta::specta]
pub(crate) async fn import_series(
    kind: ChapterKind,
    window: WebviewWindow<CefRuntime>,
    projects: State<'_, ProjectLibrary>,
    library: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<Series, Error> {
    reject_import_while_processing(&processing)?;
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
    else {
        return Err(anyhow::anyhow!("the import was cancelled").into());
    };
    let source = folder.path().to_owned();
    let projects = projects.inner().clone();
    let library = library.inner().clone();
    let planned = tokio_rayon::spawn({
        let library = library.clone();
        move || plan_series(&library, &source, kind)
    })
    .await?;

    for chapter in &planned.chapters {
        let Some(directory) = resolve_source(&planned, chapter) else {
            continue;
        };
        let files = import::collect_importable(&directory)?;
        if files.is_empty() {
            continue;
        }
        let imported = tokio_rayon::spawn(move || match kind {
            ChapterKind::Manga => import::import(files),
            ChapterKind::Webtoon => import::import_webtoon(files, Slicing::Auto),
        })
        .await?;
        let mut project = projects.create(&chapter.project).await?;
        import::apply(&mut project, imported).await?;
        tracing::info!(series = %planned.id, chapter = %chapter.project, "imported a chapter");
    }

    let mut series = planned;
    for chapter in &mut series.chapters {
        chapter.status = ChapterStatus::Ready;
    }
    library.write(&series)?;
    Ok(series)
}

/// Works out what a folder holds before anything is written: whether it is a serial or a single
/// volume, what its chapters are called, and which project each one will become.
fn plan_series(library: &SeriesLibrary, source: &Path, kind: ChapterKind) -> Result<Series> {
    let title = source
        .file_name()
        .and_then(|name| name.to_str())
        .context("the selected folder has no usable name")?
        .to_owned();
    let id = library.reserve_id(&title)?;
    let stem = sanitize(&title);
    let stem = if stem.is_empty() {
        "series".to_owned()
    } else {
        stem
    };

    let (serial, sources) = match classify_source(source)? {
        SourceShape::OneShot => (false, vec![(None, title.clone())]),
        SourceShape::Serial(children) => {
            let chapters = children
                .into_iter()
                .map(|child| {
                    let name = child
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("chapter")
                        .to_owned();
                    (Some(name.clone()), name)
                })
                .collect::<Vec<_>>();
            (true, chapters)
        }
    };

    let chapters = sources
        .into_iter()
        .enumerate()
        .map(|(index, (source, title))| SeriesChapter {
            seq: index as u32 + 1,
            project: format!("{stem} Ch{}", index + 1),
            title,
            source,
            kind,
            status: ChapterStatus::Pending,
        })
        .collect();

    Ok(Series {
        id,
        title,
        serial,
        cover: None,
        source_root: Some(source.to_owned()),
        chapters,
        settings: SeriesSettings::default(),
    })
}

/// The directory a chapter is imported from, or `None` when the series lost its source folder.
fn resolve_source(series: &Series, chapter: &SeriesChapter) -> Option<PathBuf> {
    let root = series.source_root.as_ref()?;
    Some(match &chapter.source {
        Some(relative) => root.join(relative),
        None => root.clone(),
    })
}

/// Runs a processing job over the given chapters, one after another.
///
/// The kernel allows exactly one project and one job at a time, so the batch is a serial loop:
/// open a chapter, run the whole project scope, wait for the job, move on. Every chapter commits on
/// its own, so an interrupted batch resumes by simply running the chapters that are still pending.
#[tauri::command]
#[specta::specta]
pub(crate) async fn process_series_chapters(
    id: String,
    projects: Vec<String>,
    operation: koharu_pipeline::Operation,
    handle: AppHandle<CefRuntime>,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<(), Error> {
    let library = library.inner().clone();
    let mut series = library.read(&id)?;
    let project_library = handle.state::<ProjectLibrary>().inner().clone();
    let total = projects.len();

    for (index, chapter) in projects.iter().enumerate() {
        if !series.chapters.iter().any(|entry| &entry.project == chapter) {
            return Err(anyhow::anyhow!("{chapter} is not a chapter of this series").into());
        }
        let opened = project_library.open(chapter).await?;
        replace_project(&handle, opened).await?;
        let job = process(
            handle.clone(),
            koharu_pipeline::Scope::Project,
            operation.clone(),
            handle.state::<CurrentProject>(),
            handle.state::<Processing>(),
            handle.state::<JobChannel>(),
        )
        .await?;
        tracing::info!(series = %series.id, chapter = %chapter, index = index + 1, total, "processing a chapter");
        wait_for_job(&handle.state::<Processing>(), job).await?;
        if let Some(entry) = series
            .chapters
            .iter_mut()
            .find(|entry| &entry.project == chapter)
        {
            entry.status = ChapterStatus::Done;
        }
        library.write(&series)?;
    }
    Ok(())
}

/// Waits for one job to leave the running state.
///
/// Polling rather than subscribing keeps the batch self-contained: the job channel is already
/// spoken for by the running job, and a batch only needs the terminal state.
async fn wait_for_job(processing: &Processing, job: JobId) -> Result<()> {
    loop {
        let state = processing.jobs.lock().get(&job).map(|job| job.state.clone());
        match state {
            Some(JobState::Finished) => return Ok(()),
            Some(JobState::Failed) => bail!("the chapter's job failed"),
            Some(JobState::Stopped) => bail!("the chapter's job was stopped"),
            Some(JobState::Running) | None => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

/// Exports the given chapters side by side under one chosen folder.
///
/// Like processing, this is a serial loop over projects, because only one project can be open at
/// a time. Each chapter lands in its own archive or sub-folder named after the chapter, so a whole
/// volume exports into a single directory the user picked once.
#[tauri::command]
#[specta::specta]
pub(crate) async fn export_series_chapters(
    id: String,
    projects: Vec<String>,
    format: ExportFormat,
    window: WebviewWindow<CefRuntime>,
    handle: AppHandle<CefRuntime>,
    library: State<'_, SeriesLibrary>,
    desktop: State<'_, koharu_desktop::Desktop>,
) -> std::result::Result<(), Error> {
    let library = library.inner().clone();
    let series = library.read(&id)?;
    let chapters = series
        .chapters
        .iter()
        .filter(|chapter| projects.contains(&chapter.project))
        .collect::<Vec<_>>();
    if chapters.is_empty() {
        return Err(anyhow::anyhow!("none of the selected chapters belong to this series").into());
    }
    let Some(root) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
        .map(|folder| folder.path().to_owned())
    else {
        return Ok(());
    };
    let project_library = handle.state::<ProjectLibrary>().inner().clone();

    for chapter in chapters {
        let opened = project_library.open(&chapter.project).await?;
        replace_project(&handle, opened).await?;
        let snapshot = {
            let current = handle.state::<CurrentProject>();
            let open = current.project.lock().await;
            open.as_ref()
                .context("no project is open")?
                .snapshot()
        };
        let stem = sanitize(&chapter.title);
        let destination = match format {
            ExportFormat::Cbz => root.join(format!("{stem}.cbz")),
            ExportFormat::Png | ExportFormat::Psd => root.join(&stem),
        };
        export_snapshot(snapshot, format, destination, &desktop).await?;
        tracing::info!(series = %series.id, chapter = %chapter.project, "exported a chapter");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("koharu-series-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create fixture directory");
        path
    }

    fn touch(path: &Path) {
        fs::write(path, b"x").expect("write fixture file");
    }

    #[test]
    fn a_folder_of_images_is_one_shot_and_a_folder_of_chapters_is_serial() {
        let one_shot = fixture("oneshot");
        touch(&one_shot.join("001.jpg"));
        assert!(matches!(
            classify_source(&one_shot).expect("classify"),
            SourceShape::OneShot
        ));

        let serial = fixture("serial");
        for chapter in ["Chapter 22", "Chapter 23"] {
            let directory = serial.join(chapter);
            fs::create_dir_all(&directory).expect("create chapter directory");
            touch(&directory.join("001.jpg"));
        }
        let SourceShape::Serial(chapters) = classify_source(&serial).expect("classify") else {
            panic!("a folder of chapter directories must be serial");
        };
        assert_eq!(
            chapters,
            vec![serial.join("Chapter 22"), serial.join("Chapter 23")],
            "chapters come back in natural order"
        );

        fs::remove_dir_all(&one_shot).expect("remove one-shot fixture");
        fs::remove_dir_all(&serial).expect("remove serial fixture");
    }

    #[test]
    fn a_folder_with_nothing_importable_is_refused() {
        let empty = fixture("empty");
        fs::create_dir_all(empty.join("notes")).expect("create unusable directory");
        touch(&empty.join("readme.txt"));
        assert!(classify_source(&empty).is_err());
        fs::remove_dir_all(&empty).expect("remove empty fixture");
    }

    #[test]
    fn images_win_over_subdirectories() {
        // A download that put loose images next to per-chapter folders is one chapter of loose
        // images, not a serial: the chapter folders are the leftovers of another layout.
        let mixed = fixture("mixed");
        touch(&mixed.join("001.jpg"));
        let chapter = mixed.join("Chapter 01");
        fs::create_dir_all(&chapter).expect("create chapter directory");
        touch(&chapter.join("001.jpg"));
        assert!(matches!(
            classify_source(&mixed).expect("classify"),
            SourceShape::OneShot
        ));
        fs::remove_dir_all(&mixed).expect("remove mixed fixture");
    }

    #[test]
    fn directory_names_avoid_reserved_characters_and_collisions() {
        assert_eq!(directory_name("Demo Title", |_| false).unwrap(), "Demo Title");
        assert_eq!(
            directory_name("第1季: 特别篇", |_| false).unwrap(),
            "第1季- 特别篇",
            "a colon is not usable in a directory name"
        );
        assert_eq!(directory_name("  ...  ", |_| false).unwrap(), "series");
        assert_eq!(
            directory_name("Demo Title", |name| name == "Demo Title").unwrap(),
            "Demo Title 2"
        );
    }
}
