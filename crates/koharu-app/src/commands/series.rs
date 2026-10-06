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
    import::{self, AdBandSkip, Format, Slicing},
    lifecycle::replace_project,
    output::{ExportFormat, export_snapshot},
    processing::{JobChannel, JobId, JobState, Processing, process},
    project::{CurrentProject, ProjectLibrary},
    reject_import_while_processing, reject_settings_while_processing,
};
use crate::injection::{self, Injection};

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

    #[must_use]
    pub const fn as_uuid(self) -> uuid::Uuid {
        self.0
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

/// Imports one chapter of an existing series from its source folder.
///
/// `ad` 是「沿用设置区里的值」为假时用户填的那一组高度。传 `None` 表示沿用本漫画的设置；传值表示这一次
/// 用用户的值而**不写回索引**——设置只有一份，导入完这一章之后仍然由设置区说了算
/// （`docs/series-settings-design.md` §2.3）。
#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn import_series_chapter(
    id: String,
    name: String,
    kind: ChapterKind,
    ad: Option<AdBands>,
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
    let ad = ad.unwrap_or(series.settings.ad);
    // 两个分支都归到「页面 + 广告带报告」，让调用方不必关心这一章是哪种源形态。
    let (pages, ad_bands) = tokio_rayon::spawn(move || match kind {
        ChapterKind::Manga => import::import(files).map(|pages| (pages, import::AdBandReport::default())),
        ChapterKind::Webtoon => import::import_webtoon(files, Slicing::Auto, ad)
            .map(|webtoon| (webtoon.imported, webtoon.ad_bands)),
    })
    .await?;
    let project_name = format!("{stem} Ch{seq}");
    let mut project = projects.create(&project_name).await?;
    report_ad_bands(&series.id, &ad_bands);
    import::apply(&mut project, pages).await?;

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
        move || plan_series(&library, &source, kind)
    })
    .await?;
    // 导入循环读的就是这份设置，所以对话框给的高度在循环开始前就位。
    planned.settings.ad = ad;

    for chapter in &planned.chapters {
        let Some(directory) = resolve_source(&planned, chapter) else {
            continue;
        };
        let files = import::collect_importable(&directory)?;
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
        report_ad_bands(&planned.id, &ad_bands);
        import::apply(&mut project, pages).await?;
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
/// 窗口都各不相同，而本地模型只读一次盘：重建阶段运行器不重建翻译器。整个批次跑完后句柄恢复成用户
/// 配置，漫画的资料不会漏进设置页。
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

    let assets = SeriesAssets::collect(&library, &series, &project_library).await?;
    let baseline = koharu_pipeline::PipelineConfig::load()?.read()?.clone();
    let mut report = SeriesRun::default();
    for (index, chapter) in projects.iter().enumerate() {
        let opened = project_library.open(chapter).await?;
        replace_project(&handle, opened).await?;
        let prior = injection::load_prior_context(&assets.directory, assets.seq(chapter)?);
        let mut built = assets.injection();
        built.global = baseline.translation.instructions.clone();
        let rendered = injection::render(&built, baseline.translation.model.provider);
        // 窗口的页数与章外那一段都是运行参数而不是资料：它们决定翻译阶段每次请求带多少先例，所以
        // 与附加说明一起写进管线配置，由 `koharu-pipeline` 在每页上就地取一个跨章连续的窗口。
        apply_injection(&handle, rendered.text, assets.context_pages, prior)?;
        report.matched = rendered.matched as u32;
        report.unsupported = rendered.unsupported;

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
    restore_user_config(&handle)?;
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

/// 一部漫画的翻译资料，按批量开始前的状态一次性备好。
///
/// **预扫描是必需的，不是优化。** 术语命中要看整部漫画的原文，而内核同一时刻只有一个活动项目，所以
/// 「拿到全部原文」只能靠批量开始前逐章打开一次；上一章的基线也出自同一趟。两趟合一，每章两次打开，
/// 与是否启用上文无关。
struct SeriesAssets {
    /// 漫画目录，上文旁挂文件与它同级。
    directory: PathBuf,
    /// 用户手写的指导。
    guidance: String,
    glossary: Glossary,
    /// 整部漫画的原文，供术语命中判定。
    haystack: String,
    /// 上文窗口的页数：章内与章外取的都是「最近若干页」，用户只调一个旋钮。
    context_pages: u32,
    /// 章序号 → 章项目名，用来判「下一章」和读自己的上文。
    order: Vec<(u32, String)>,
}

impl SeriesAssets {
    /// 逐章打开一次，收集全部原文，并为每一章的下一章写下上文窗口的基线。
    ///
    /// 基线取自「磁盘上已有的译文」，所以中断之后重跑一批仍然拿得到上一章的写法。
    async fn collect(
        library: &SeriesLibrary,
        series: &Series,
        projects: &ProjectLibrary,
    ) -> Result<Self> {
        let directory = library.path(&series.id);
        // 术语表坏了就报错，不静默当成空表：用户以为在生效的术语表不见了，比一次失败更难排查。
        let glossary = crate::glossary::load(&directory)?;
        let mut assets = Self {
            directory,
            guidance: series.settings.guidance.clone(),
            glossary,
            haystack: String::new(),
            context_pages: series.settings.context_pages,
            order: series
                .chapters
                .iter()
                .map(|chapter| (chapter.seq, chapter.project.clone()))
                .collect(),
        };
        for chapter in &series.chapters {
            // 还没建项目的章没有原文可读，跳过而不是替它造一份空的。
            let Ok(opened) = projects.open(&chapter.project).await else {
                continue;
            };
            let snapshot = opened.snapshot();
            assets.haystack.push_str(&injection::source_text(&snapshot)?);
            let Some(next) = assets.successor(&chapter.project) else {
                continue;
            };
            let context = injection::prior_chapter_context(&snapshot, assets.context_pages);
            injection::save_prior_context(&assets.directory, next, &context)?;
        }
        Ok(assets)
    }

    /// 某章的下一章序号。序号是作品结构的客观顺序，与用户勾选和执行的顺序无关
    /// （`docs/reference/koharu-glossary-design.md` §2.4）。
    fn successor(&self, project: &str) -> Option<u32> {
        let seq = self.seq(project).ok()?;
        self.order
            .iter()
            .map(|(candidate, _)| *candidate)
            .find(|candidate| *candidate > seq)
    }

    fn seq(&self, project: &str) -> Result<u32> {
        self.order
            .iter()
            .find(|(_, candidate)| candidate == project)
            .map(|(seq, _)| *seq)
            .context("the chapter is not registered in this series")
    }

    /// 这一章的注入内容：只有人工资料。全局指导由调用方补上，因为它是用户的而不是这部漫画的。
    ///
    /// 上文不在这里：它逐页变化，由翻译阶段就地取一个跨章连续的窗口，所以走配置而不是走渲染。
    fn injection(&self) -> Injection {
        Injection {
            guidance: self.guidance.clone(),
            glossary: self.glossary.clone(),
            haystack: self.haystack.clone(),
            global: None,
            order: None,
        }
    }
}

/// 把这一章的注入内容与上文窗口写进管线跑的那份配置。
///
/// 写的是**配置**而不是换一条管线：换管线会连翻译器一起重建，本地模型的权重要重读一遍盘，而重建阶段
/// 运行器与翻译器共用同一个已加载模型。空串表示这一章没有可注入的内容，于是这一章就用回用户自己的
/// 全局指导，而不是把字段清空。
fn apply_injection(
    handle: &AppHandle<CefRuntime>,
    text: String,
    context_pages: u32,
    prior: Vec<Vec<koharu_translator::TranslationContext>>,
) -> Result<()> {
    let live = handle.state::<koharu_config::Config<koharu_pipeline::PipelineConfig>>();
    let mut current = live.write()?;
    current.translation.instructions = (!text.is_empty()).then_some(text);
    current.translation.context_pages = context_pages;
    current.translation.prior_chapter_context = prior;
    Ok(())
}

/// 把管线跑的那份配置恢复成用户设置。
///
/// 重新读一次而不是回滚到批开始时的快照：批量期间用户改了设置的话，恢复成旧快照会把那次改动从管线里
/// 抹掉，尽管它已经在文件里了。
fn restore_user_config(handle: &AppHandle<CefRuntime>) -> Result<()> {
    let user = koharu_pipeline::PipelineConfig::load()?.read()?.clone();
    let live = handle.state::<koharu_config::Config<koharu_pipeline::PipelineConfig>>();
    let mut current = live.write()?;
    *current = user;
    Ok(())
}

/// 当前活动项目的场景。章刚被替换成活动项目，所以这一次拿到的就是它自己的。
async fn current_snapshot(handle: &AppHandle<CefRuntime>) -> Result<koharu_scene::Snapshot> {
    let current = handle.state::<CurrentProject>();
    let project = current.project.lock().await;
    Ok(project.as_ref().context("no project is open")?.snapshot())
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
}
