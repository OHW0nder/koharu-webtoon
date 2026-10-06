//! 漫画层：一部漫画作为一组章项目的索引，再加上漫画自己的数据。
//!
//! 内核只认项目（`.khrproj`），漫画不是内核概念，而是这一层维护的索引。两者不是解耦的：**每一个章
//! 项目都恰好被一部漫画的一个章条目认领**，所以删除一路收在这一层——删章连带删它的项目，删漫画连带
//! 删全部章项目。曾经存在过的「未认领的普通项目」这一类已经取消（`docs/series-management-design.md`
//! §1），因为它让同一件事有两种归属方式，也让章项目的删除只有一半。
//!
//! 索引放在漫画目录里，因此它跟着漫画一起被移动、备份和删除。章在索引里只记项目名，因为
//! 项目名是内核唯一稳定的公开标识；章项目在内核眼中仍然是普通项目，能被单独打开、单独
//! 导出、单独跑流水线。
//!
//! 漫画目录就是项目根目录下一个不带 `.khrproj` 后缀的文件夹，所以内核的项目枚举会自然跳过
//! 它们，两套目录并存不需要改内核。

use std::{
    collections::BTreeSet,
    io::Write as _,
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
    import::{self, AdBandSkip, Format, Slicing},
    lifecycle::{close_current_project, replace_project},
    output::{self, ExportFormat},
    processing::{JobChannel, Processing, Subject, start_job},
    project::{CurrentProject, ProjectLibrary, ProjectSummary},
    reject_import_while_processing, reject_settings_while_processing,
};
use crate::injection;

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
    pub chapters: Vec<SeriesChapter>,
    pub settings: SeriesSettings,
}

/// Reads the series-level translation settings.
///
/// Split from `get_series` because the settings panel is the only caller that needs them: a
/// chapter list has no reason to drag the glossary file name along with it.
#[tracing::instrument(
    target = "koharu_metrics",
    name = "series_settings_read",
    skip_all,
    fields(origin = "user", series = %id),
)]
#[tauri::command]
#[specta::specta]
pub(crate) fn get_series_settings(
    id: String,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<SeriesSettings, Error> {
    let series = library.read(&id)?;
    Ok(series.settings)
}

/// Writes the series-level settings back into the index.
///
/// Only the settings field is replaced. The index is the one file that describes the whole series,
/// so writing back a copy the caller assembled from scratch would silently drop chapters, the
/// cover or the source folder it never meant to touch.
#[tracing::instrument(
    target = "koharu_metrics",
    name = "series_settings_written",
    skip_all,
    fields(
        origin = "user",
        series = %id,
        head = settings.ad.head,
        tail = settings.ad.tail,
        has_guidance = !settings.guidance.trim().is_empty(),
        context_pages = settings.context_pages,
    ),
)]
#[tauri::command]
#[specta::specta]
pub(crate) fn set_series_settings(
    id: String,
    settings: SeriesSettings,
    library: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<SeriesSettings, Error> {
    reject_settings_while_processing(&processing)?;
    let mut series = library.read(&id)?;
    series.settings = settings;
    library.write(&series)?;
    Ok(series.settings)
}

/// Reads the glossary stored beside the series index.
///
/// A missing file is an empty table rather than a failure: `settings.glossary` records the file
/// name, but a glossary nobody ever wrote is the normal state of a freshly imported series, and the
/// editor should open on an empty list rather than on an error it would have to special-case.
#[tracing::instrument(
    target = "koharu_metrics",
    name = "glossary_read",
    skip_all,
    fields(origin = "user", series = %id),
)]
#[tauri::command]
#[specta::specta]
pub(crate) fn get_glossary(
    id: String,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<Glossary, Error> {
    let directory = library.path(&id);
    if !directory.join(GLOSSARY_FILE).is_file() {
        return Ok(Glossary::default());
    }
    Ok(crate::glossary::load(&directory)
        .with_context(|| format!("failed to read the glossary of series {id:?}"))?)
}

/// Saves the glossary and points the index at it.
///
/// The file is written before the index on purpose. The index only records the file name, so the
/// other order can leave it naming a file that was never written, and the next read would come back
/// empty for a glossary the user just saved.
#[tracing::instrument(
    target = "koharu_metrics",
    name = "glossary_written",
    skip_all,
    fields(origin = "user", series = %id, entries = glossary.entries.len()),
)]
#[tauri::command]
#[specta::specta]
pub(crate) fn set_glossary(
    id: String,
    glossary: Glossary,
    library: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<Glossary, Error> {
    reject_settings_while_processing(&processing)?;
    validate_glossary(&glossary)
        .with_context(|| format!("the glossary of series {id:?} cannot be saved"))?;
    let directory = library.path(&id);
    crate::glossary::save(&directory, &glossary)
        .with_context(|| format!("failed to write the glossary of series {id:?}"))?;

    let mut series = library.read(&id)?;
    series.settings.glossary = Some(GLOSSARY_FILE.to_owned());
    library.write(&series)?;
    Ok(glossary)
}

/// 漫画级的翻译资料与配置。
///
/// 归属漫画而不是章：资料的生命周期跟着整部作品走，而章项目会被删除重建。
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SeriesSettings {
    /// 条漫首尾的站点广告高度，导入时从源图裁掉。
    pub ad: AdBands,
    /// 本作特有的翻译风格约定，手写散文。
    ///
    /// 空串表示没有。全局指导仍然保留，排在它之后作为跨作品的个人口味兜底。
    pub guidance: String,
    /// 术语表文件名，与索引同目录。`None` 表示还没有术语表。
    ///
    /// 存文件名而不是内容：术语表条目多且每条都可能被编辑，放进索引会让改一个词条就要重写
    /// 整个编排文件。`Some` 但文件不存在是合法状态，读取时当作空表。
    pub glossary: Option<String>,
    /// 章内上文回溯的页数：翻译第 N 页时带上第 N−1 到第 N−N 页的成对双语对照。0 表示不注入。
    ///
    /// 这不是章的资料而是**运行参数**——它决定翻译阶段每次请求带多少先例，所以归到批量执行时
    /// 写进管线配置，而不像指导与术语表那样渲染进附加说明。上文窗口跨章连续，所以这一个值同时是
    /// 章内与章外的回溯距离：翻到本章第 1 页时窗口整段来自上一章末尾，翻到第 2 页时让出一页换成本章
    /// 第 1 页，以此类推。合适的距离取决于模型与作品，只能实测确定，所以它是用户可调的；硬上限由
    /// `koharu_pipeline::MAX_CONTEXT_PAGES` 兜住。
    #[serde(default = "default_context_pages")]
    pub context_pages: u32,
}

/// 上文窗口的默认页数，与 `koharu-pipeline` 的默认值一致。
fn default_context_pages() -> u32 {
    4
}

/// `Default` 是手写的而不是派生的，因为派生的 `u32` 会给 0，而 0 的语义是「不注入章内上文」——
/// 新漫画与读不到该字段的旧索引都应当落到 4 这个可用值上，而不是静默变成功能关闭。
impl Default for SeriesSettings {
    fn default() -> Self {
        Self {
            ad: AdBands::default(),
            guidance: String::new(),
            glossary: None,
            context_pages: default_context_pages(),
        }
    }
}

/// 条漫首尾的广告带高度，单位为原始源图像素。
///
/// 两条高度各自锚定在源图的一端，**不锚定在顶端**。每章的总高不同，从顶端起算的位置会随
/// 章节长度漂移；锚定底端才能让同一个数值在整部作品里通用。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AdBands {
    /// 自源图顶端起算的首条高度。0 表示这一端没有广告。
    pub head: u32,
    /// 自源图底端起算的尾条高度。0 表示这一端没有广告。
    pub tail: u32,
}

impl AdBands {
    /// 这次导入是否需要裁剪。
    pub(crate) fn is_empty(self) -> bool {
        self.head == 0 && self.tail == 0
    }

    /// 一批源图里第 `index` 张实际承担的那一端广告带，`last` 是这一批最后一张的下标。
    ///
    /// 站点广告长在整章的首图顶部与末图底部，中间的图两侧都是正文。给每张图都套上两条广告带会把首图
    /// 的尾部和末图的头部这两段正文一起削掉，所以每张图只取自己那一端；只有单图的一批两端都落在它
    /// 自己身上。
    pub(crate) fn for_page(self, index: usize, last: usize) -> Self {
        Self {
            head: if index == 0 { self.head } else { 0 },
            tail: if index == last { self.tail } else { 0 },
        }
    }
}

/// 术语表。
///
/// 结构逐字段对齐上游社区实现（见 `docs/series-translation-assets-audit.md` §5），将来上游若
/// 实现同类功能，迁移是字段对字段的复制而不是语义猜测。只加一个本地扩展字段：备注。
#[derive(Clone, Debug, Default, Serialize, Deserialize, Type)]
pub struct Glossary {
    /// 关闭时整份表不参与注入，条目保留。
    pub enabled: bool,
    pub source_language: Option<String>,
    pub target_language: Option<String>,
    /// 源文本指纹。用于判断这份表是否还对得上当前项目的原文。
    pub source_fingerprint: Option<String>,
    pub entries: Vec<GlossaryEntry>,
}

impl Glossary {
    /// 归一化之后重复的「原文 + 类别」组合。导入与人工编辑都要过这一关。
    pub fn duplicate_keys(&self) -> Vec<(String, GlossaryKind)> {
        let mut seen = BTreeSet::new();
        let mut duplicates = Vec::new();
        for entry in &self.entries {
            let key = (normalize_glossary_source(&entry.source), entry.kind);
            if !seen.insert(key.clone()) {
                duplicates.push(key);
            }
        }
        duplicates
    }
}

/// 一条术语。
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GlossaryEntry {
    pub id: GlossaryEntryId,
    /// 原文术语。
    pub source: String,
    /// 定稿译文。`None` 表示还没定稿，注入时跳过。
    pub translation: Option<String>,
    pub kind: GlossaryKind,
    /// 关闭时保留条目但不参与注入。
    pub enabled: bool,
    /// 适用情形说明，例如「只在战斗场景指武器」。上游没有这个字段。
    pub note: String,
    pub confidence: Option<f32>,
    pub occurrence_count: u32,
    pub examples: Vec<String>,
    pub source_origin: GlossaryValueOrigin,
    pub translation_origin: Option<GlossaryValueOrigin>,
    /// 最近一次扫描时是否仍出现在原文里。
    pub present_in_last_scan: bool,
}

impl GlossaryEntry {
    /// 是否参与注入：启用、有译文、译文非空。
    pub fn is_injectable(&self) -> bool {
        self.enabled
            && self.translation.as_ref().is_some_and(|value| !value.trim().is_empty())
    }
}

/// 术语的稳定标识。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, Type)]
#[serde(transparent)]
#[specta(transparent)]
pub struct GlossaryEntryId(#[specta(type = String)] uuid::Uuid);

impl GlossaryEntryId {
    #[must_use]
    pub fn new() -> Self {
        Self(uuid::Uuid::now_v7())
    }
}

impl Default for GlossaryEntryId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for GlossaryEntryId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for GlossaryEntryId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

/// 术语的类别。类别只用于界面筛选与排序，不进入提示词。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GlossaryKind {
    Person,
    Place,
    Organization,
    Item,
    Ability,
    Term,
    Other,
}

/// 这个值是谁写的。人工写的一律不被自动流程覆盖。
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GlossaryValueOrigin {
    Detected,
    Automatic,
    User,
    Imported,
}

/// 术语原文的归一化形式，用于去重与匹配。
///
/// NFKC 归一化后折叠空白并转小写。NFKC 是必要的：漫画文本里全角与半角拉丁混用，不归一化就会
/// 把同一个词当成两个。
#[must_use]
pub fn normalize_glossary_source(source: &str) -> String {
    let normalized = icu_normalizer::ComposingNormalizerBorrowed::new_nfkc()
        .normalize_iter(source.chars())
        .collect::<String>();
    normalized
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// 校验一份术语表是否可以落盘。
pub fn validate_glossary(glossary: &Glossary) -> Result<()> {
    let mut ids = BTreeSet::new();
    for entry in &glossary.entries {
        if !ids.insert(entry.id) {
            bail!("glossary entry ids contain duplicates");
        }
        if entry.source.trim().is_empty() {
            bail!("a glossary entry has an empty source");
        }
        if let Some(confidence) = entry.confidence
            && !(0.0..=1.0).contains(&confidence)
        {
            bail!("glossary confidence must be within 0..=1");
        }
    }
    let duplicates = glossary.duplicate_keys();
    if !duplicates.is_empty() {
        bail!(
            "glossary has {} duplicated source and kind combinations",
            duplicates.len()
        );
    }
    Ok(())
}

/// 术语表文件名，与索引同目录。
pub(crate) const GLOSSARY_FILE: &str = "glossary.json";

/// 漫画里的一章。
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SeriesChapter {
    pub seq: u32,
    pub title: String,
    /// 章项目名，对应 `<root>/<project>.khrproj`。
    pub project: String,
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

    /// 根目录下的全部索引。
    ///
    /// 单部漫画的索引缺失或损坏时跳过它，而不是让整个列表查不出来：它仍然出现在漫画柜里，只是这一轮
    /// 不参与。
    fn indices(&self) -> Result<Vec<Series>> {
        let entries =
            fs::read_dir(&self.root).with_context(|| format!("failed to read {}", self.root.display()))?;
        let mut found = Vec::new();
        for entry in entries.filter_map(|entry| entry.ok()) {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Some(id) = entry
                .path()
                .file_stem()
                .and_then(|id| id.to_str())
                .map(str::to_owned)
            else {
                continue;
            };
            if let Ok(series) = self.read(&id) {
                found.push(series);
            }
        }
        Ok(found)
    }

    /// 列出全部漫画，按目录最近修改时间倒序，与项目列表的排序保持一致。
    pub(crate) fn list(&self) -> Result<Vec<SeriesSummary>> {
        let mut entries = self
            .indices()?
            .into_iter()
            .filter_map(|series| {
                let touched = fs::metadata(self.path(&series.id)).ok()?.modified().ok()?;
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
    /// 章项目与漫画共用项目根目录、同一套 `.khrproj` 实体，所以「有没有主人」完全取决于索引
    /// 里有没有它。与漫画索引一比，差集就是孤立项目（见 [`list_orphaned_projects`]）。
    pub(crate) fn claimed_projects(&self) -> Result<BTreeSet<String>> {
        Ok(self
            .indices()?
            .into_iter()
            .flat_map(|series| series.chapters.into_iter().map(|chapter| chapter.project))
            .collect())
    }

    /// 反查这个项目属于哪部漫画。
    ///
    /// 章在索引里只记项目名，所以归属就是一次字符串匹配。每个项目都属于且只属于一部漫画
    /// （`docs/series-management-design.md` §1），所以查不到只可能是章项目已经被删掉而索引没跟上——那是
    /// 损坏的索引，而不是一种合法的「独立项目」。
    pub(crate) fn owning_series(&self, project: &str) -> Result<Option<Series>> {
        Ok(self
            .indices()?
            .into_iter()
            .find(|series| series.chapters.iter().any(|chapter| chapter.project == project)))
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

    /// 漫画目录。注入层把资料与旁挂文件挂在它下面，所以那层拿目录而不是重新推一遍位置。
    pub(crate) fn path(&self, id: &str) -> PathBuf {
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
    // 目录名是下载器按数字命名的，原始的字节序会把 `Ch100` 排在 `Ch02` 前面。导入层早就用
    // alphanumeric_sort 处理同一件事，这里不该另写一套。
    alphanumeric_sort::sort_slice_by_os_str_key(&mut children, |path| {
        path.file_name().unwrap_or_else(|| path.as_os_str())
    });    if children.is_empty() {
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

/// 下一个可用的章序号。
///
/// **序号是一个位置，不是计数器。** 用户删掉导错的第 20 章之后会重新导入同一话，那一章要拿回 20
/// ——否则它会排到末尾，而前文取的是「紧邻的上一章」，位置错了上下文就跟着错。所以这里补最小的
/// 空洞，没有空洞才往上接。
///
/// 序号不连续本身不是缺陷。用户可以故意跳过某一章而不导入，那一章的位置就该空着；翻译与前文都
/// 按现有章的顺序走，系统不去纠正这个选择。
fn next_seq(chapters: &[SeriesChapter]) -> u32 {
    let taken = chapters
        .iter()
        .map(|chapter| chapter.seq)
        .collect::<BTreeSet<_>>();
    // 上界是 `max + 1` 而不是 `len + 1`：序号可以稀疏，而**每一个**空洞都在 `max + 1` 之前，所以
    // 扫到这里一定有答案。
    let ceiling = taken.iter().next_back().map_or(1, |seq| seq.saturating_add(1));
    (1..=ceiling)
        .find(|seq| !taken.contains(seq))
        .expect("the ceiling sits above every taken slot")
}

/// 把广告带的执行结果写成一条日志。
///
/// 跳过不是错误，但用户需要知道它发生了：广告高度设得比某些源图允许的更大时，那几张图会整张
/// 进项目、带着广告，等于这一批的设置对它们无效。静默地导入会让用户以为设置生效了。
fn report_ad_bands(series: &str, report: &import::AdBandReport) {
    for skipped in report.skipped() {
        let reason = match skipped.reason {
            AdBandSkip::Covered => "the bands cover the whole image",
            AdBandSkip::TooShort => "what is left is shorter than one page",
            AdBandSkip::NotSliceable => "what is left is already one page",
        };
        tracing::warn!(
            series,
            page = %skipped.name,
            reason,
            "the series' ad bands were not applied to this image"
        );
    }
    if report.trimmed() > 0 {
        tracing::info!(
            series,
            trimmed = report.trimmed(),
            "cut the ad bands off before slicing"
        );
    }
}

/// 把一个下载好的章节文件夹并入这部漫画。
///
/// **章名就是文件夹名。** 用户从生肉站点手动下载，那个文件夹名是他唯一表达的意图；应用不追问
/// 「这是第几话」，只按名字把它放进正确的位置。序号由全体章名决定，所以中途插入一章、或者删掉
/// 中间一章，都不需要用户心算它该是第几话。
///
/// `ad` 是「沿用设置区里的值」为假时用户填的那一组高度。传 `None` 表示沿用本漫画的设置；传值表示
/// 这一次用用户的值而**不写回索引**——设置只有一份，导入完这一章之后仍然由设置区说了算
/// （`docs/series-settings-design.md` §2.3）。
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_series_chapter(
    id: String,
    directory: PathBuf,
    kind: ChapterKind,
    ad: Option<AdBands>,
    projects: State<'_, ProjectLibrary>,
    library: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<Series, Error> {
    reject_import_while_processing(&processing)?;
    let projects = projects.inner().clone();
    let mut series = library.read(&id)?;

    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .context("the chosen folder has no usable name")?
        .to_owned();
    if series.chapters.iter().any(|chapter| chapter.title == name) {
        return Err(anyhow::anyhow!("{name} is already part of this series").into());
    }
    let files = import::collect_importable(&directory)?;
    if files.is_empty() {
        return Err(anyhow::anyhow!("{name} holds no importable pages").into());
    }

    let ad = ad.unwrap_or(series.settings.ad);
    // 两个分支都归到「页面 + 广告带报告」，让调用方不必关心这一章是哪种源形态。
    let (pages, ad_bands) = tokio_rayon::spawn(move || match kind {
        ChapterKind::Manga => {
            import::import(files).map(|pages| (pages, import::AdBandReport::default()))
        }
        ChapterKind::Webtoon => import::import_webtoon(files, Slicing::Auto, ad)
            .map(|webtoon| (webtoon.imported, webtoon.ad_bands)),
    })
    .await?;

    // 序号补最小的空洞，所以删掉第 20 章之后重新导入它会拿回 20，自然落回 19 与 21 之间。
    let seq = next_seq(&series.chapters);
    let stem = sanitize(&series.title);
    let stem = if stem.is_empty() { "series" } else { &stem };
    let project_name = format!("{stem} Ch{seq}");
    series.chapters.push(SeriesChapter {
        seq,
        title: name.clone(),
        project: project_name.clone(),
        kind,
        status: ChapterStatus::Ready,
    });

    let mut project = projects.create(&project_name).await?;
    let outcome = async {
        report_ad_bands(&series.id, &ad_bands);
        import::apply(&mut project, pages).await?;
        library.write(&series)
    }
    .await;
    if let Err(error) = outcome {
        // 项目建好而索引没写进去，这一章就成了没有主人的残骸；它连候选列表都进不去，重试会撞上
        // 同名项目。删掉它，失败的那一趟才真的没有留下东西。
        rollback_projects(&projects, std::slice::from_ref(&project_name)).await;
        series
            .chapters
            .retain(|chapter| !(chapter.project == project_name && chapter.title == name));
        return Err(error.into());
    }
    Ok(series)
}

/// 关掉正在打开的章项目，然后删掉它。
///
/// 活动项目不能直接删：内核里还持有一份打开的场景，所以先走正常的关闭路径。
async fn delete_chapter_project(
    handle: &AppHandle<CefRuntime>,
    project: &str,
    library: ProjectLibrary,
) -> Result<()> {
    let active = handle
        .state::<CurrentProject>()
        .project
        .lock()
        .await
        .as_ref()
        .is_some_and(|open| open.name == project);
    if active {
        close_current_project(handle).await?;
    }
    // 阻塞任务要 `'static`，所以名字得自己拥有，不能把借用送进去。
    let owned = project.to_owned();
    tokio::task::spawn_blocking(move || library.delete(&owned))
        .await
        .context("project deletion task failed")?
}

/// 删掉一章，连同它的章项目。
///
/// **删除的语义是「这一章导错了，之后会重新导入同一话」，所以项目必须一起删。** 重导的项目名是
/// `<漫画名> Ch<序号>`，与被删的那个同名；留着它，`projects.create` 会直接撞名，于是「重新导入」这条路
/// 走不通（`docs/series-management-design.md` §1）。
///
/// **其余章的序号一个都不动。** 序号是 `context-<序号>.json` 的键，也是「下一章」判定的依据，重排会让
/// 已有的译文上文错位到错误的章。删中间一章之后序号出现空洞，那正是「这里少了一话」的可读表示；下一个
/// 导入的章节由 [`next_seq`] 把这个空洞补回去，于是重导的那一章拿回原来的位置，上文也就仍然来自它
/// 前面那一章。
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn delete_series_chapter(
    id: String,
    project: String,
    handle: AppHandle<CefRuntime>,
    projects: State<'_, ProjectLibrary>,
    library: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<Series, Error> {
    reject_import_while_processing(&processing)?;
    let mut series = library.read(&id)?;
    // 索引里已经没有这一章，就是这次删除要达成的状态。批量删除中途失败时剩下的那几章靠重试收敛，
    // 而报「不是这部漫画的章」会让调用方以为参数错了，去改一个本来就没错的东西。
    let Some(chapter) = series
        .chapters
        .iter()
        .find(|chapter| chapter.project == project)
        .cloned()
    else {
        tracing::info!(series = %id, chapter = %project, "the chapter was already gone");
        return Ok(series);
    };
    delete_chapter_project(&handle, &project, projects.inner().clone()).await?;
    // 其余章的序号一个都不动。删掉第 20 章之后 21 仍然是 21：位置属于用户的作品结构，系统不替他
    // 补位——他重新导入第 20 章时，那个空洞正好留给它。
    series.chapters.retain(|entry| entry.project != project);
    library.write(&series)?;
    tracing::info!(series = %id, chapter = %project, seq = chapter.seq, "deleted a chapter");
    Ok(series)
}

/// 删掉一部漫画，连同它的全部章项目。
///
/// **章项目必须一起删。** 一个章项目只能属于一部漫画（`docs/series-management-design.md` §1），而「没有被
/// 任何漫画认领的项目」这一类已经取消。留着它们只会得到一批没有归属的项目——既不进漫画柜，也删不掉。
///
/// 译文是唯一无法从源目录重建的东西，所以界面上必须先讲清不可逆的范围。
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn delete_series(
    id: String,
    handle: AppHandle<CefRuntime>,
    projects: State<'_, ProjectLibrary>,
    library: State<'_, SeriesLibrary>,
    processing: State<'_, Processing>,
) -> std::result::Result<(), Error> {
    reject_import_while_processing(&processing)?;
    let directory = library.path(&id);
    // 目录已经不在就是这次删除要达成的状态，所以重试要能收敛。再读一次索引只会得到
    // 「找不到路径」——一个已经把目标达成的情况，却报成失败，而调用方无从判断该不该重试。
    if !directory.is_dir() {
        tracing::info!(series = %id, "the series was already gone");
        return Ok(());
    }
    let series = library.read(&id)?;
    let projects = projects.inner().clone();
    for chapter in &series.chapters {
        delete_chapter_project(&handle, &chapter.project, projects.clone()).await?;
    }
    let shown = directory.clone();
    tokio::task::spawn_blocking(move || fs::remove_dir_all(&directory))
        .await
        .context("series deletion task failed")?
        .with_context(|| format!("failed to delete {}", shown.display()))?;
    tracing::info!(series = %id, chapters = series.chapters.len(), "deleted a series");
    Ok(())
}

/// Opens a folder picker for one chapter and returns its path, or `None` when cancelled.
///
/// The picker lives here rather than in the frontend because `rfd` needs the window handle, and the
/// path it returns is the chapter's name as well as its source — so this is the one question the
/// import flow has to ask the operating system.
#[tauri::command]
#[specta::specta]
pub(crate) async fn pick_chapter_folder(
    window: WebviewWindow<CefRuntime>,
) -> std::result::Result<Option<String>, Error> {
    Ok(rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
        .map(|folder| folder.path().to_string_lossy().into_owned()))
}

/// 列出没有任何漫画认领的章项目。
///
/// **「未认领」不是一类合法对象，而是残骸。** 每个项目都恰好属于一部漫画的一个章
/// （`docs/series-management-design.md` §1），所以能被列出来的只有两种来源：导入在写索引之前
/// 中断了，或者索引被手工删掉了。它们进不了漫画柜，也没有别的入口能删——正是这里补上的那个。
///
/// 叫「孤立项目」而不是「未分组项目」：后者听起来像一个可以继续编辑的地方，而这里的东西没有
/// 主人，只能清理掉。
#[tauri::command]
#[specta::specta]
pub(crate) fn list_orphaned_projects(
    projects: State<'_, ProjectLibrary>,
    series: State<'_, SeriesLibrary>,
) -> std::result::Result<Vec<ProjectSummary>, Error> {
    let claimed = series.claimed_projects()?;
    let orphans = projects
        .list()?
        .into_iter()
        .filter(|project| !claimed.contains(&project.name))
        .collect();
    Ok(orphans)
}

/// 删掉一个孤立项目。
///
/// 守卫是它确实孤立：被认领的项目必须先从它那一章删掉，否则索引里会留下一个打不开的章条目，
/// 而那正是这个入口最初要收拾的烂摊子，不该由它再制造一次。
#[tauri::command]
#[specta::specta]
pub(crate) fn delete_orphaned_project(
    name: String,
    projects: State<'_, ProjectLibrary>,
    series: State<'_, SeriesLibrary>,
) -> std::result::Result<(), Error> {
    if series.owning_series(&name)?.is_some() {
        return Err(anyhow::anyhow!("{name} belongs to a series; delete that chapter instead").into());
    }
    projects.delete(&name)?;
    tracing::info!(project = %name, "deleted an orphaned project");
    Ok(())
}

/// 重新指定源目录。
///
/// **只换目录，不自动导入。** 换源之后新目录里的章名可能与已登记的 `source` 撞名，而自动导入会把正在
/// 正常工作的章重导一遍；让用户点「导入新章」、在候选列表里看到文件数之后再确认。
///
#[tauri::command]
#[specta::specta]
pub(crate) fn rename_series(
    id: String,
    title: String,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<Series, Error> {
    let title = title.trim();
    if title.is_empty() {
        return Err(anyhow::anyhow!("a series title cannot be blank").into());
    }
    let mut series = library.read(&id)?;
    let renamed = directory_name(title, |candidate| {
        candidate != id && library.path(candidate).exists()
    })
    .with_context(|| format!("{title:?} cannot be used as a directory name"))?;
    if renamed != id {
        let from = library.path(&id);
        fs::rename(&from, library.path(&renamed))
            .with_context(|| format!("failed to rename {}", from.display()))?;
    }
    series.id = renamed.clone();
    series.title = title.to_owned();
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
///
/// **广告高度由对话框给出。** 这是唯一无处可存的一次：漫画尚不存在，索引里还没有地方放这个值，
/// 之后一律从设置区管理（`docs/series-settings-design.md` §2.3）。
#[tauri::command]
#[specta::specta]
pub(crate) async fn import_series(
    kind: ChapterKind,
    ad: AdBands,
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
    let mut planned = tokio_rayon::spawn({
        let library = library.clone();
        let source = source.clone();
        move || plan_series(&library, &source, kind)
    })
    .await?;
    // 导入循环读的就是这份设置，所以对话框给的高度在循环开始前就位。
    planned.settings.ad = ad;

    let mut created = Vec::new();
    // The chapters are read out of the folder the user picked, one directory per planned chapter:
    // a single volume is that folder itself, a serial is each of its chapter sub-folders.
    let directories = chapter_directories(&source, &planned);
    let outcome = build_chapter_projects(&projects, &planned, &directories, kind).await;
    match outcome {
        Ok(names) => created = names,
        Err(error) => {
            // 建了一半的项目没有被任何索引认领，而未被认领的项目已经不是一类合法对象。
            // 不回滚的话它们既进不了漫画柜、也没有别的入口能删，于是永久留在盘上。
            let error = error.into();
            rollback_projects(&projects, &created).await;
            return Err(error);
        }
    }

    let mut series = planned;
    for chapter in &mut series.chapters {
        chapter.status = ChapterStatus::Ready;
    }
    // 索引写失败同样要回滚：项目已经全部建好，此时它们同样没有主人。
    if let Err(error) = library.write(&series) {
        rollback_projects(&projects, &created).await;
        return Err(error.into());
    }
    Ok(series)
}

/// 每个计划中的章从哪个目录读。
///
/// 单行本的目录就是它自己，连载则是它下面的每一个章子目录——与 [`classify_source`] 用的判据一致，
/// 所以这里只是把那个判据的结果重新摆成一份按下标对得上的清单。
fn chapter_directories(source: &Path, planned: &Series) -> Vec<PathBuf> {
    if planned.chapters.len() == 1 && !planned.serial {
        return vec![source.to_owned()];
    }
    planned
        .chapters
        .iter()
        .map(|chapter| source.join(&chapter.title))
        .collect()
}

/// 逐章建项目，交出已经建成的那些名字。
///
/// 建成功一个就登记一个，所以调用方拿到的清单恰好覆盖「已经落盘、因此需要回滚」的范围。
/// 切页与导入本身都可能失败（磁盘满、源文件被占用），而失败点在登记之后，所以登记必须紧跟
/// 在 `create` 成功之后而不是攒到最后。
async fn build_chapter_projects(
    projects: &ProjectLibrary,
    planned: &Series,
    directories: &[PathBuf],
    kind: ChapterKind,
) -> Result<Vec<String>> {
    let mut created = Vec::new();
    for (chapter, directory) in planned.chapters.iter().zip(directories) {
        let files = import::collect_importable(directory)?;
        if files.is_empty() {
            continue;
        }
        let ad = planned.settings.ad;
        // 两个分支都归到「页面 + 广告带报告」，让调用方不必关心这一章是哪种源形态。
        let (pages, ad_bands) = tokio_rayon::spawn(move || match kind {
            ChapterKind::Manga => {
                import::import(files).map(|pages| (pages, import::AdBandReport::default()))
            }
            ChapterKind::Webtoon => import::import_webtoon(files, Slicing::Auto, ad)
                .map(|webtoon| (webtoon.imported, webtoon.ad_bands)),
        })
        .await?;
        let mut project = projects.create(&chapter.project).await?;
        created.push(chapter.project.clone());
        report_ad_bands(&planned.id, &ad_bands);
        import::apply(&mut project, pages).await?;
        tracing::info!(series = %planned.id, chapter = %chapter.project, "imported a chapter");
    }
    Ok(created)
}

/// 删掉这一趟已经建出来的章项目。
///
/// **删不掉的只记日志。** 主错误才是用户要解决的那一个，而一个残留目录不该盖掉它；何况回滚
/// 本身失败时，用户仍然有一条路：漫画柜里那个「孤立项目」入口（`list_orphaned_projects`）正是
/// 为这些残骸准备的。
async fn rollback_projects(projects: &ProjectLibrary, created: &[String]) {
    for name in created.iter().rev() {
        let library = projects.clone();
        let owned = name.clone();
        match tokio::task::spawn_blocking(move || library.delete(&owned)).await {
            Ok(Ok(())) => tracing::warn!(project = %name, "rolled back a chapter project"),
            Ok(Err(error)) => {
                tracing::error!(project = %name, %error, "could not roll back a chapter project")
            }
            Err(error) => tracing::error!(project = %name, %error, "the rollback task was dropped"),
        }
    }
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

    let (serial, titles) = match classify_source(source)? {
        SourceShape::OneShot => (false, vec![title.clone()]),
        SourceShape::Serial(children) => (
            true,
            children
                .into_iter()
                .map(|child| {
                    child
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("chapter")
                        .to_owned()
                })
                .collect(),
        ),
    };

    let chapters = titles
        .into_iter()
        .enumerate()
        .map(|(index, title)| SeriesChapter {
            seq: index as u32 + 1,
            project: format!("{stem} Ch{}", index + 1),
            title,
            kind,
            status: ChapterStatus::Pending,
        })
        .collect();

    Ok(Series {
        id,
        title,
        serial,
        cover: None,
        chapters,
        settings: SeriesSettings::default(),
    })
}

/// 一次批量实际注入进去的东西。
///
/// 只需要报命中数：上文不再走注入通道，它的总量由用户自己设的 `context_pages` 页数上限约束，不存在
/// 静默截断，因此没有「被丢掉多少」需要解释。
#[derive(Clone, Copy, Debug, Default, Serialize, Type)]
pub struct SeriesRun {
    /// 跑完的章数。
    pub chapters: u32,
    /// 术语表里出现在原文中的条数。整批共用同一份原文，所以这个值在批内不变。
    pub matched: u32,
    /// 当前服务商不接受提示词，这一批的注入内容全部无效。
    pub unsupported: bool,
}

/// Runs a processing job over the given chapters, one after another.
///
/// The kernel allows exactly one project and one job at a time, so the batch is a serial loop:
/// open a chapter, run the whole project scope, wait for the job, move on. Every chapter commits on
/// its own, so an interrupted batch resumes by simply running the chapters that are still pending.
///
/// **注入内容在每章开始前换一次。** 换的动作是写管线跑的那份内存配置，所以每章的指导、术语命中与上文
/// 窗口都各不相同，而本地模型只读一次盘：重建阶段运行器不重建翻译器。还原由 [`start_job`] 在每章跑完后
/// 做，漫画的资料因此不会漏进设置页。
#[tauri::command]
#[specta::specta]
pub(crate) async fn process_series_chapters(
    id: String,
    projects: Vec<String>,
    operation: koharu_pipeline::Operation,
    handle: AppHandle<CefRuntime>,
    library: State<'_, SeriesLibrary>,
) -> std::result::Result<SeriesRun, Error> {
    let library = library.inner().clone();
    let mut series = library.read(&id)?;
    let project_library = handle.state::<ProjectLibrary>().inner().clone();
    let total = projects.len();
    for chapter in &projects {
        if !series.chapters.iter().any(|entry| &entry.project == chapter) {
            return Err(anyhow::anyhow!("{chapter} is not a chapter of this series").into());
        }
    }

    let assets = injection::Assets::collect(&library, &series, &project_library).await?;
    let baseline = koharu_pipeline::PipelineConfig::load()?.read()?.clone();
    let mut report = SeriesRun::default();
    for (index, chapter) in projects.iter().enumerate() {
        let opened = project_library.open(chapter).await?;
        replace_project(&handle, opened).await?;
        let prepared = injection::Prepared::for_chapter(&assets, chapter, &baseline)?;
        report.matched = prepared.matched;
        report.unsupported = prepared.unsupported;

        let started = start_job(
            handle.clone(),
            koharu_pipeline::Scope::Project,
            operation.clone(),
            handle.state::<Processing>(),
            handle.state::<JobChannel>(),
            Subject::ThisChapter(prepared),
        )
        .await?;
        tracing::info!(series = %series.id, chapter = %chapter, index = index + 1, total, "processing a chapter");
        // 章名进消息：跑批会一路停在出错的那一章，没有章名的报错在界面上等于没有报错。
        if let Err(error) = started.terminal().await {
            return Err(anyhow::anyhow!("{chapter}: {error:#}").into());
        }
        // 这一章刚跑出来的译文比预扫描时鲜，覆盖写给下一章，同一批里排在后面的章因此能吃到。
        if let Some(next) = assets.successor(chapter) {
            let snapshot = current_snapshot(&handle).await?;
            let fresh = injection::prior_chapter_context(&snapshot, assets.context_pages);
            injection::save_prior_context(&assets.directory, next, &fresh)?;
        }
        if let Some(entry) = series
            .chapters
            .iter_mut()
            .find(|entry| &entry.project == chapter)
        {
            entry.status = ChapterStatus::Done;
        }
        library.write(&series)?;
        report.chapters += 1;
    }
    tracing::info!(
        target = "koharu_metrics",
        metric = "series_run",
        series = %series.id,
        chapters = report.chapters,
        matched = report.matched,
        outcome = if report.unsupported { "provider_ignores_instructions" } else { "injected" },
    );
    Ok(report)
}

/// 当前活动项目的场景。章刚被替换成活动项目，所以这一次拿到的就是它自己的。
async fn current_snapshot(handle: &AppHandle<CefRuntime>) -> Result<koharu_scene::Snapshot> {
    let current = handle.state::<CurrentProject>();
    let project = current.project.lock().await;
    Ok(project.as_ref().context("no project is open")?.snapshot())
}

/// Exports the chosen chapters as one archive that keeps the shelf's shape.
///
/// **A volume exports as a single `.cbz` whose layout mirrors the shelf:** the series name, then a
/// folder per chapter, then that chapter's pages. Unpacking it gives back
/// `Demo Title/Ch10/00101.jpg`, so the archive reads the way the project does. Exporting one chapter
/// on its own is a different deliverable and stays flat — that archive *is* the chapter.
///
/// **The file name carries the range.** `Demo Title ch10-ch21.cbz` says which chapters are inside
/// without opening it, and the numbers are the chapters' own slots, so a range like `ch10-ch12` in
/// an archive that skips 11 is visible rather than hidden.
///
/// Rendering is a serial loop because the kernel holds one open project at a time, and each chapter
/// is written into the archive as soon as it is rendered — the whole volume is never in memory at once.
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
    let title = sanitize(&series.title);
    let title = if title.is_empty() { "series" } else { &title };

    let first = chapters.first().map(|chapter| chapter.seq).unwrap_or(0);
    let last = chapters.last().map(|chapter| chapter.seq).unwrap_or(0);
    let stem = if first == last {
        format!("{title} ch{first}")
    } else {
        format!("{title} ch{first}-ch{last}")
    };

    let destination = root.join(format!("{stem}.cbz"));
    // One archive for the whole selection, so the writer outlives the loop.
    let staged = tempfile::NamedTempFile::new_in(&root)?;
    let mut archive = zip::ZipWriter::new(staged);

    for chapter in &chapters {
        let opened = project_library.open(&chapter.project).await?;
        replace_project(&handle, opened).await?;
        let snapshot = current_snapshot(&handle).await?;
        let pages = output::render_pages(snapshot, format, &desktop).await?;
        let folder = format!("{}/{}", title, entry_name(&chapter.title));
        for (name, bytes) in pages {
            // PNG data is already compressed.
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            archive.start_file(format!("{folder}/{name}"), options)?;
            archive.write_all(&bytes)?;
        }
        tracing::info!(series = %series.id, chapter = %chapter.project, "exported a chapter");
    }
    archive.finish()?.persist(&destination)?;
    tracing::info!(series = %series.id, chapters = chapters.len(), "exported a volume");
    Ok(())
}

/// 一个章名在归档路径里的样子。
///
/// 归档条目用 `/` 分隔，所以名字里的斜杠会凭空多出一层目录——那正是导出结构被打乱的原因。
fn entry_name(name: &str) -> String {
    let flattened: String = name
        .chars()
        .map(|character| match character {
            '/' | '\\' => '-',
            other => other,
        })
        .collect();
    let trimmed = flattened.trim().trim_end_matches(['.', ' ']);
    if trimmed.is_empty() { "chapter".to_owned() } else { trimmed.to_owned() }
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

    /// 造一个项目目录：`ProjectLibrary::list` 认的是 `.khrproj` 后缀加 `state-*.khr`。
    fn project_fixture(root: &Path, name: &str) {
        let directory = root.join(format!("{name}.khrproj"));
        fs::create_dir_all(&directory).expect("create project directory");
        touch(&directory.join("state-a.khr"));
    }

    #[test]
    fn a_project_no_series_claims_is_reported_as_orphaned() {
        // 导入在写索引之前中断，或者索引被手工删掉，都会留下这种项目：它不被任何漫画认领，
        // 于是进不了漫画柜，也没有任何别的入口能删它。
        let root = fixture("orphans");
        let library = SeriesLibrary { root: root.clone() };
        for name in ["Demo Title Ch1", "Demo Title Ch2", "Blue Archive Ch1"] {
            project_fixture(&root, name);
        }

        // 一部认领了前两章的漫画：索引写成之后它们就不再是残骸。
        let source = root.join("Demo Title");
        fs::create_dir_all(source.join("Ch1")).expect("create chapter directory");
        fs::create_dir_all(source.join("Ch2")).expect("create chapter directory");
        touch(&source.join("Ch1").join("001.jpg"));
        touch(&source.join("Ch2").join("001.jpg"));
        let mut series = plan_series(&library, &source, ChapterKind::Manga).expect("plan a series");
        series.chapters.truncate(2);
        library.write(&series).expect("write the index");

        let claimed = library.claimed_projects().expect("collect claimed");
        assert!(claimed.contains("Demo Title Ch1"));
        assert!(claimed.contains("Demo Title Ch2"));
        assert!(
            !claimed.contains("Blue Archive Ch1"),
            "a project no index names is an orphan, not a chapter"
        );

        fs::remove_dir_all(&root).expect("remove fixture directory");
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

    #[test]
    fn a_missing_context_pages_field_reads_as_a_usable_default() {
        // 派生的 `u32` 默认会给 0，而 0 的语义是「不注入章内上文」——那会让升级过的漫画静默失去
        // 这个功能，用户看不出任何异常。
        assert_eq!(SeriesSettings::default().context_pages, 4);
        let older: SeriesSettings =
            serde_json::from_str(r#"{"ad":{"head":0,"tail":0},"guidance":"","glossary":null}"#)
                .expect("an index written before the field existed");
        assert_eq!(older.context_pages, 4);

        // 显式写 0 是用户的选择，必须尊重。
        let disabled: SeriesSettings =
            serde_json::from_str(r#"{"ad":{"head":0,"tail":0},"guidance":"","glossary":null,"context_pages":0}"#)
                .expect("an index that turned the feature off");
        assert_eq!(disabled.context_pages, 0);
    }

    /// 造一章只为占住一个序号。
    fn chapter(seq: u32) -> SeriesChapter {
        SeriesChapter {
            seq,
            title: format!("Ch{seq}"),
            project: format!("Ch{seq}"),
            kind: ChapterKind::Manga,
            status: ChapterStatus::Ready,
        }
    }

    #[test]
    fn a_deleted_chapter_leaves_a_slot_the_next_import_reclaims() {
        // 用户在一部 25 话的连载里删掉了导错的第 20 章。21 章的序号**不动**：位置属于用户的作品
        // 结构，系统不替他补位——他想重新导入第 20 章时，那个空洞正好留给它。
        let mut chapters: Vec<SeriesChapter> = (1..=25).map(chapter).collect();
        chapters.retain(|entry| entry.seq != 20);
        assert_eq!(
            chapters.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            (1..=19).chain([21, 22, 23, 24, 25]).collect::<Vec<_>>(),
            "21 stays 21, so 19 and 21 sit next to each other with a gap between their numbers"
        );

        // 重新导入第 20 章：它拿回 20，于是自然落回 19 和 21 之间，前文重新接上。
        assert_eq!(next_seq(&chapters), 20, "the gap takes the next chapter");

        // 没有空洞时往下接：导入第 26 章排在 25 之后。
        let whole: Vec<SeriesChapter> = (1..=25).map(chapter).collect();
        assert_eq!(next_seq(&whole), 26);
    }

    #[test]
    fn chapters_are_ordered_by_number_not_by_text() {
        // 章目录是下载器按数字命名的，所以 `Ch100` 必须排在 `Ch02` 之后。目录项的原始字节序做不到
        // 这一点，而错误的顺序会直接决定每章拿到哪个序号。
        let serial = fixture("ordering");
        for chapter in ["Ch100", "Ch02", "Ch2", "Ch24", "Ch1"] {
            let directory = serial.join(chapter);
            fs::create_dir_all(&directory).expect("create chapter directory");
            touch(&directory.join("001.jpg"));
        }
        let SourceShape::Serial(chapters) = classify_source(&serial).expect("classify") else {
            panic!("a folder of chapter directories must be serial");
        };
        let names = chapters
            .iter()
            .map(|path| path.file_name().and_then(|name| name.to_str()).expect("utf-8"))
            .collect::<Vec<_>>();
        assert_eq!(names, ["Ch1", "Ch2", "Ch02", "Ch24", "Ch100"]);

        fs::remove_dir_all(&serial).expect("remove fixture directory");
    }
}