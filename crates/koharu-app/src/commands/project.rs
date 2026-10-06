use std::{
    collections::HashSet,
    io::Cursor,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, anyhow, bail};
use image::{DynamicImage, ImageFormat, RgbaImage};
use koharu_desktop::Frame;
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, Authored, Commit, EntityId, EntityOrigin,
    Geometry as SceneGeometry, Group as SceneGroup, Origin, PageDraft, Point as ScenePoint,
    Presents, RasterLayer as SceneRasterLayer, RasterLayerKind, Region as SceneRegion,
    RemovePolicy, Revision, Session, Snapshot, SourceText as SceneSourceText,
    TextGroup as SceneTextGroup, TextLayout as SceneTextLayout, TextLayoutKind,
    Translation as SceneTranslation, Typography as SceneTypography, Visibility as SceneVisibility,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use tokio::sync::Mutex;

use super::{
    canvas::Point,
    editing::{GeometryUpdate, TypographyUpdate},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RasterStrokeMode {
    Paint,
    Erase,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ProjectInfo {
    pub reference: ChapterRef,
    pub label: String,
    pub revision: Revision,
    pub active_page: Option<EntityId>,
    pub can_undo: bool,
    pub can_redo: bool,
}

/// 一章的身份：漫画目录名 + 章目录名。
///
/// 内核认不出这两个名字——它只收一个路径，项目身份是 `create` 时生成的 `DocumentId`，与路径和
/// 名字都无关。所以路径推导是这一层的事，而推导所需的全部信息就是这两段目录名。合成一个结构，
/// 章项目路径便只有一个出处。
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Type)]
pub struct ChapterRef {
    /// 漫画目录名，库根目录的直接子目录。
    pub series: String,
    /// 章目录名，漫画目录下的直接子目录。
    pub chapter: String,
}

/// 漫画库的位置。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryConfig {
    /// 漫画库根目录，漫画目录是它的直接子目录。
    ///
    /// `None` 是「用户没有指定」。没指定时用哪个位置属于库自己的知识，而不是配置模式的默认值，
    /// 所以解析留给 [`ProjectLibrary`]：配置只如实保存用户说了什么。
    pub root: Option<PathBuf>,
}

impl LibraryConfig {
    pub(crate) fn load() -> Result<koharu_config::Config<Self>> {
        koharu_config::load("library")
    }
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
pub struct PageSize {
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct PageSummary {
    pub id: EntityId,
    pub label: String,
    pub size: PageSize,
    pub source_asset: Option<String>,
    #[specta(type = f64)]
    pub layer_count: usize,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Page {
    pub id: EntityId,
    pub label: String,
    pub size: PageSize,
    pub layers: Vec<Layer>,
    pub regions: Vec<AnalysisRegion>,
}

#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Layer {
    Group {
        id: EntityId,
        parent: Option<EntityId>,
        visibility: LayerVisibility,
        name: String,
        role: Option<GroupRole>,
    },
    Text {
        id: EntityId,
        parent: Option<EntityId>,
        geometry: Option<Geometry>,
        angle_degrees: Option<f32>,
        visibility: LayerVisibility,
        content: Box<TextContent>,
        typography: Option<Typography>,
        layout: TextLayoutKind,
        automatic_region: Option<EntityId>,
    },
    Raster {
        id: EntityId,
        parent: Option<EntityId>,
        visibility: LayerVisibility,
        image: Option<String>,
        name: String,
        kind: RasterLayerKind,
    },
    Image {
        id: EntityId,
        parent: Option<EntityId>,
        geometry: Geometry,
        visibility: LayerVisibility,
        image: String,
    },
    Artwork {
        id: EntityId,
        parent: Option<EntityId>,
        geometry: Geometry,
        visibility: LayerVisibility,
        image: String,
    },
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum GroupRole {
    Text,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct TextContent {
    pub id: EntityId,
    pub source: Option<SourceText>,
    pub translation: Option<Translation>,
    pub role: Option<String>,
    pub source_region: Option<EntityId>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct AnalysisRegion {
    pub id: EntityId,
    pub parent: Option<EntityId>,
    pub geometry: Geometry,
    pub kind: String,
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Geometry {
    pub points: Vec<Point>,
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
pub struct LayerVisibility {
    pub visible: bool,
    pub opacity: f32,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SourceText {
    pub text: String,
    pub language: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Translation {
    pub text: String,
    pub language: Option<String>,
}

#[derive(Clone, Debug, serde::Deserialize, Serialize, Type)]
pub struct Typography {
    pub preferred_font: Option<String>,
    pub font_weight: Option<u16>,
    pub font_style: Option<koharu_scene::FontStyle>,
    pub size: Option<f32>,
    pub auto_fit: bool,
    pub color: Option<[u8; 4]>,
    pub stroke_color: Option<[u8; 4]>,
    pub stroke_width: Option<f32>,
    pub alignment: Option<koharu_scene::TextAlignment>,
    pub writing_mode: Option<koharu_scene::WritingMode>,
}

pub(crate) struct CurrentProject {
    pub(crate) project: Mutex<Option<Project>>,
}

#[derive(Clone)]
pub(crate) struct ProjectLibrary {
    root: PathBuf,
}

impl ProjectLibrary {
    pub(crate) fn new() -> Result<Self> {
        let root = Self::resolve_root(LibraryConfig::load()?.read()?.root.clone())?;
        std::fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        Ok(Self { root })
    }

    /// 用户指定的位置优先，没指定才用平台默认。
    ///
    /// 「没指定时去哪」是库自己的知识，所以解析留在这里而不是塞进配置的默认值：`Default` 拿不到
    /// `Result`，硬写一个兜底会把 Documents 不可用这件事安静掉，而那正是启动失败最常见的原因。
    fn resolve_root(configured: Option<PathBuf>) -> Result<PathBuf> {
        let Some(root) = configured else {
            return dirs::document_dir()
                .context("the Documents directory is unavailable; choose a library folder instead")
                .map(|documents| documents.join("Koharu"));
        };
        if root.as_os_str().is_empty() {
            bail!("the library folder cannot be empty");
        }
        Ok(root)
    }

    /// 章项目目录的枚举，按最近使用排序。
    ///
    /// 只在找「没有任何漫画认领的章」时需要：每个章都恰好属于一部漫画，所以这个集合与漫画索引
    /// 一比，差集就是残骸——导入在写索引之前中断，或索引被手工删掉之后留下的东西。
    ///
    /// 遍历两层而不查索引，因为索引可能正好是缺的那一份，而这一层存在的意义就是收拾那种残骸。
    /// 认章的依据是目录形状而非任何后缀：漫画目录与章目录不在同一层，后缀已经没有区分作用。
    pub(crate) fn list(&self) -> Result<Vec<ChapterRef>> {
        let mut found = Vec::new();
        for series in std::fs::read_dir(&self.root)
            .with_context(|| format!("failed to read {}", self.root.display()))?
        {
            let series = series?;
            if !series.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let Some(series_name) = series.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            for chapter in std::fs::read_dir(series.path())
                .with_context(|| format!("failed to read {}", series.path().display()))?
            {
                let chapter = chapter?;
                if !chapter.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                if !is_project_directory(&chapter.path()) {
                    continue;
                }
                let Some(chapter_name) = chapter.file_name().to_str().map(str::to_owned) else {
                    continue;
                };
                found.push((
                    last_used(&chapter.path()),
                    ChapterRef {
                        series: series_name.clone(),
                        chapter: chapter_name,
                    },
                ));
            }
        }
        found.sort_unstable_by(|(left_used, left), (right_used, right)| {
            right_used.cmp(left_used).then_with(|| {
                left.series
                    .to_lowercase()
                    .cmp(&right.series.to_lowercase())
                    .then_with(|| {
                        left.chapter
                            .to_lowercase()
                            .cmp(&right.chapter.to_lowercase())
                    })
            })
        });
        Ok(found.into_iter().map(|(_, reference)| reference).collect())
    }

    pub(crate) async fn create(
        &self,
        reference: &ChapterRef,
        label: String,
    ) -> Result<Project> {
        Project::create(reference.clone(), label, self.resolve(reference)?).await
    }

    pub(crate) async fn open(&self, reference: &ChapterRef, label: String) -> Result<Project> {
        Project::open(reference.clone(), label, self.resolve(reference)?).await
    }

    pub(crate) fn delete(&self, reference: &ChapterRef) -> Result<()> {
        let path = self.resolve(reference)?;
        if !path.is_dir() {
            bail!("{}/{chapter:?} does not exist", reference.series, chapter = reference.chapter);
        }
        std::fs::remove_dir_all(&path)
            .with_context(|| format!("failed to delete {}", path.display()))
    }

    /// 章项目目录：`<root>/<漫画目录>/<章目录>`。
    ///
    /// 两段各自校验，所以引用越不出漫画目录，漫画目录也越不出库根目录——`ChapterRef` 是从前端
    /// 来的，不能假定它已经在磁盘上被验证过。
    fn resolve(&self, reference: &ChapterRef) -> Result<PathBuf> {
        let series =
            validate_directory_name(&reference.series).context("the series directory name")?;
        let chapter =
            validate_directory_name(&reference.chapter).context("the chapter directory name")?;
        Ok(self.root.join(series).join(chapter))
    }

    /// 库根目录。漫画目录是它的直接子目录，章项目在漫画目录之下，所以漫画层要的是这个根而不是
    /// 自己再推一遍位置：根只有一处决定。
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

/// 一个目录是不是章项目。只认内核写的状态文件，所以索引、术语表、旁挂文件都自然被排除。
fn is_project_directory(path: &Path) -> bool {
    path.join("state-a.khr").is_file() || path.join("state-b.khr").is_file()
}

/// 章项目最近一次写入的时间，取两个状态槽里较新的那个。
fn last_used(path: &Path) -> std::time::SystemTime {
    ["state-a.khr", "state-b.khr"]
        .into_iter()
        .filter_map(|slot| std::fs::metadata(path.join(slot)).ok()?.modified().ok())
        .max()
        .unwrap_or(std::time::UNIX_EPOCH)
}

pub(crate) struct Project {
    pub(crate) session: Session,
    /// 这一章是谁。路径推导、归属反查、以及「已经打开这一章」的判断都用它。
    pub(crate) reference: ChapterRef,
    /// 人类可读的名。漫画名已经在父目录里，所以它补的是章标题，导出文件名靠它区分同一部漫画的各章。
    pub(crate) label: String,
    pub(crate) active_page: Option<EntityId>,
    pub(crate) undo: Vec<Vec<Revision>>,
    pub(crate) redo: Vec<Vec<Revision>>,
}

impl Project {
    pub(crate) async fn create(
        reference: ChapterRef,
        label: String,
        path: PathBuf,
    ) -> Result<Self> {
        let session = Session::create(&path)
            .await
            .with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self::new(session, reference, label))
    }

    pub(crate) async fn open(
        reference: ChapterRef,
        label: String,
        path: PathBuf,
    ) -> Result<Self> {
        let session = Session::open(&path)
            .await
            .with_context(|| format!("failed to open {}", path.display()))?;
        Ok(Self::new(session, reference, label))
    }

    fn new(session: Session, reference: ChapterRef, label: String) -> Self {
        let active_page = session.snapshot().pages().next().map(|page| page.id());
        Self {
            session,
            reference,
            label,
            active_page,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    pub(crate) fn snapshot(&self) -> Snapshot {
        self.session.snapshot()
    }

    pub(crate) fn revision(&self) -> Revision {
        self.snapshot().revision()
    }

    pub(crate) fn active_page(&self) -> Option<EntityId> {
        self.active_page
    }

    pub(crate) fn select_page(&mut self, page: EntityId) -> Result<()> {
        self.snapshot().page(page)?;
        self.active_page = Some(page);
        Ok(())
    }

    pub(crate) fn reconcile_page(&mut self) {
        let snapshot = self.snapshot();
        if self
            .active_page
            .is_none_or(|page| snapshot.page(page).is_err())
        {
            self.active_page = snapshot.pages().next().map(|page| page.id());
        }
    }

    pub(crate) fn info(&self) -> ProjectInfo {
        ProjectInfo {
            reference: self.reference.clone(),
            label: self.label.clone(),
            revision: self.revision(),
            active_page: self.active_page,
            can_undo: !self.undo.is_empty(),
            can_redo: !self.redo.is_empty(),
        }
    }

    pub(crate) fn pages(snapshot: &Snapshot) -> Result<Vec<PageSummary>> {
        snapshot
            .pages()
            .map(|page| {
                let value = page.page()?;
                let source_asset = Self::asset_id(snapshot, page.id(), "source")?;
                let layer_count = snapshot
                    .descendants(page.id())?
                    .map(|entity| Self::is_content_layer(snapshot, entity.id()))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .filter(|present| *present)
                    .count()
                    + usize::from(source_asset.is_some());
                Ok(PageSummary {
                    id: page.id(),
                    label: value.label,
                    size: PageSize {
                        width: value.width,
                        height: value.height,
                    },
                    source_asset,
                    layer_count,
                })
            })
            .collect()
    }

    pub(crate) async fn rename_page(&mut self, page: EntityId, label: String) -> Result<Commit> {
        let snapshot = self.snapshot();
        let current = snapshot.page(page)?.page()?;
        let patch = snapshot.patch(|edit| {
            edit.set_page(page, PageDraft::new(label, current.width, current.height))
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn delete_pages(&mut self, pages: Vec<EntityId>) -> Result<Commit> {
        let snapshot = self.snapshot();
        let pages = Self::unique_roots(&snapshot, pages)?;
        let patch = snapshot.patch(|edit| {
            for page in pages {
                edit.remove_entity(page, RemovePolicy::Cascade)?;
            }
            Ok(())
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn move_page(&mut self, page: EntityId, index: usize) -> Result<Commit> {
        let snapshot = self.snapshot();
        let siblings = snapshot.pages().map(|page| page.id()).collect::<Vec<_>>();
        let at = Self::placement(&siblings, page, index);
        let patch = snapshot.patch(|edit| edit.move_entity(page, None, at))?;
        self.commit(patch).await
    }

    pub(crate) async fn add_point_text(
        &mut self,
        page: EntityId,
        point: Point,
    ) -> Result<(Commit, EntityId)> {
        if !point.x.is_finite() || !point.y.is_finite() {
            bail!("text position must contain finite coordinates");
        }
        let value = self.snapshot().page(page)?.page()?;
        if point.x < 0.0 || point.y < 0.0 || point.x >= value.width || point.y >= value.height {
            bail!("text position must be inside the page");
        }
        self.add_text(
            page,
            Frame {
                x: point.x as f32,
                y: point.y as f32,
                width: (value.width - point.x).clamp(1.0, 320.0) as f32,
                height: (value.height - point.y).clamp(1.0, 120.0) as f32,
                angle_degrees: 0.0,
            },
            TextLayoutKind::Point,
        )
        .await
    }

    pub(crate) async fn add_text_box(
        &mut self,
        page: EntityId,
        frame: Frame,
    ) -> Result<(Commit, EntityId)> {
        self.add_text(page, frame, TextLayoutKind::Paragraph).await
    }

    async fn add_text(
        &mut self,
        page: EntityId,
        frame: Frame,
        kind: TextLayoutKind,
    ) -> Result<(Commit, EntityId)> {
        let snapshot = self.snapshot();
        let geometry = Self::geometry_from_frame(frame)?;
        let mut layer = None;
        let patch = snapshot.patch(|edit| {
            let content = edit.add_text_content(page, At::End)?;
            edit.set(
                content,
                &SceneSourceText {
                    text: Authored::user(String::new()),
                    language: None,
                },
            )?;
            let added_layer = edit.add_text_layer(
                page,
                At::End,
                content,
                &SceneTextLayout {
                    origin: Origin::User,
                    kind,
                    angle_degrees: Some(frame.angle_degrees),
                },
            )?;
            layer = Some(added_layer);
            edit.set(added_layer, &geometry)?;
            edit.set(
                added_layer,
                &SceneTypography {
                    origin: Origin::User,
                    preferred_font: None,
                    font_weight: None,
                    font_style: None,
                    size: None,
                    auto_fit: true,
                    color: None,
                    stroke_color: None,
                    stroke_width: None,
                    alignment: match kind {
                        TextLayoutKind::Point => Some(koharu_scene::TextAlignment::Start),
                        TextLayoutKind::Paragraph => None,
                    },
                    writing_mode: None,
                    extensions: Default::default(),
                },
            )?;
            Ok(())
        })?;
        Ok((
            self.commit(patch).await?,
            layer.expect("text layer was added while building the patch"),
        ))
    }

    pub(crate) async fn set_source_text(
        &mut self,
        layer: EntityId,
        text: String,
    ) -> Result<Commit> {
        let snapshot = self.snapshot();
        let content = Self::text_content(&snapshot, layer)?;
        let language = snapshot
            .component::<SceneSourceText>(content)?
            .and_then(|source| source.language);
        let patch = snapshot.patch(|edit| {
            edit.promote_entity_to_user(layer)?;
            edit.promote_entity_to_user(content)?;
            edit.set(
                content,
                &SceneSourceText {
                    text: Authored::user(text),
                    language,
                },
            )
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn set_translation(
        &mut self,
        layer: EntityId,
        text: Option<String>,
    ) -> Result<Commit> {
        let snapshot = self.snapshot();
        let content = Self::text_content(&snapshot, layer)?;
        let language = snapshot
            .component::<SceneTranslation>(content)?
            .and_then(|translation| translation.language);
        let patch = snapshot.patch(|edit| {
            edit.promote_entity_to_user(layer)?;
            edit.promote_entity_to_user(content)?;
            match text {
                Some(text) => edit.set(
                    content,
                    &SceneTranslation {
                        text: Authored::user(text),
                        language,
                    },
                ),
                None => edit.remove::<SceneTranslation>(content),
            }
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn set_typography(
        &mut self,
        updates: Vec<TypographyUpdate>,
    ) -> Result<Commit> {
        let snapshot = self.snapshot();
        let updates = updates
            .into_iter()
            .map(|update| {
                let content = Self::text_content(&snapshot, update.layer)?;
                Ok((update, content))
            })
            .collect::<Result<Vec<_>>>()?;
        let patch = snapshot.patch(|edit| {
            for (update, content) in updates {
                edit.promote_entity_to_user(update.layer)?;
                edit.promote_entity_to_user(content)?;
                edit.set(
                    update.layer,
                    &SceneTypography {
                        origin: Origin::User,
                        preferred_font: update.typography.preferred_font,
                        font_weight: update.typography.font_weight,
                        font_style: update.typography.font_style,
                        size: update.typography.size.filter(|size| *size > 0.0),
                        auto_fit: update.typography.auto_fit,
                        color: update.typography.color,
                        stroke_color: update.typography.stroke_color,
                        stroke_width: update.typography.stroke_width,
                        alignment: update.typography.alignment,
                        writing_mode: update.typography.writing_mode,
                        extensions: Default::default(),
                    },
                )?;
            }
            Ok(())
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn set_geometry(&mut self, updates: Vec<GeometryUpdate>) -> Result<Commit> {
        let snapshot = self.snapshot();
        let updates = updates
            .into_iter()
            .map(|update| {
                let layout = snapshot
                    .component::<SceneTextLayout>(update.layer)?
                    .context("only text layers can change geometry")?;
                let content = Self::text_content(&snapshot, update.layer)?;
                if update.points.is_none()
                    && snapshot
                        .text_layer(update.layer)?
                        .automatic_target()?
                        .is_none()
                {
                    bail!("only automatically placed text can reset its geometry");
                }
                Ok((update, content, layout))
            })
            .collect::<Result<Vec<_>>>()?;
        let patch = snapshot.patch(|edit| {
            for (update, content, mut layout) in updates {
                edit.promote_entity_to_user(update.layer)?;
                edit.promote_entity_to_user(content)?;
                layout.origin = Origin::User;
                layout.angle_degrees = None;
                edit.set(update.layer, &layout)?;
                match update.points {
                    Some(points) => edit.set(
                        update.layer,
                        &SceneGeometry {
                            origin: Origin::User,
                            points: points
                                .into_iter()
                                .map(|point| ScenePoint {
                                    x: point.x,
                                    y: point.y,
                                })
                                .collect(),
                        },
                    )?,
                    None => edit.remove::<SceneGeometry>(update.layer)?,
                }
            }
            Ok(())
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn set_visibility(
        &mut self,
        layers: Vec<EntityId>,
        visible: Option<bool>,
        opacity: Option<f32>,
    ) -> Result<Commit> {
        let snapshot = self.snapshot();
        let patch = snapshot.patch(|edit| {
            for layer in layers {
                let mut value =
                    snapshot
                        .component::<SceneVisibility>(layer)?
                        .unwrap_or(SceneVisibility {
                            origin: Origin::User,
                            visible: true,
                            opacity: 1.0,
                        });
                if let Some(visible) = visible {
                    value.visible = visible;
                }
                if let Some(opacity) = opacity {
                    value.opacity = opacity;
                }
                value.origin = Origin::User;
                edit.set(layer, &value)?;
            }
            Ok(())
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn delete_layers(&mut self, layers: Vec<EntityId>) -> Result<Commit> {
        let snapshot = self.snapshot();
        let mut expanded = Vec::new();
        for layer in layers {
            if snapshot.component::<SceneTextGroup>(layer)?.is_some() {
                expanded.extend(snapshot.children(layer)?);
            } else {
                expanded.push(layer);
            }
        }
        let layers = Self::unique_roots(&snapshot, expanded)?;
        let mut orphaned_contents = Vec::new();
        for layer in &layers {
            let Some(relation) = snapshot.relation_from::<Presents>(*layer)? else {
                continue;
            };
            let content = relation.value().target;
            if snapshot.relations_to_as::<Presents>(content).count() == 1 {
                orphaned_contents.push(content);
            }
        }
        let patch = snapshot.patch(|edit| {
            for layer in layers {
                if snapshot.page(layer).is_ok() {
                    return Err(koharu_scene::Error::Invalid(
                        "delete pages with delete_pages".to_owned(),
                    ));
                }
                edit.remove_entity(layer, RemovePolicy::Cascade)?;
            }
            for content in orphaned_contents {
                if snapshot.entity(content).is_ok() {
                    edit.remove_entity(content, RemovePolicy::Cascade)?;
                }
            }
            Ok(())
        })?;
        self.commit(patch).await
    }

    #[tracing::instrument(level = "info", skip_all, fields(layer = %layer, parent = %parent))]
    pub(crate) async fn move_layer(
        &mut self,
        layer: EntityId,
        parent: EntityId,
        index: usize,
    ) -> Result<Commit> {
        let snapshot = self.snapshot();
        let siblings = snapshot
            .children(parent)?
            .map(|candidate| Ok(Self::is_layer(&snapshot, candidate)?.then_some(candidate)))
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        let at = Self::placement(&siblings, layer, index);
        let patch = snapshot.patch(|edit| {
            edit.promote_entity_to_user(layer)?;
            if snapshot.component::<SceneTextGroup>(parent)?.is_some() {
                edit.promote_entity_to_user(parent)?;
            }
            edit.move_entity(layer, Some(parent), at)
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn set_geometries(
        &mut self,
        geometries: impl IntoIterator<Item = (EntityId, SceneGeometry, f32)>,
    ) -> Result<Commit> {
        let snapshot = self.snapshot();
        let geometries = geometries
            .into_iter()
            .map(|(element, geometry, angle_degrees)| {
                let mut layout = snapshot
                    .component::<SceneTextLayout>(element)?
                    .context("only text layers can change geometry")?;
                layout.origin = Origin::User;
                layout.angle_degrees = Some(angle_degrees);
                let content = Self::text_content(&snapshot, element)?;
                Ok((element, geometry, content, layout))
            })
            .collect::<Result<Vec<_>>>()?;
        let patch = snapshot.patch(|edit| {
            for (element, mut geometry, content, layout) in geometries {
                edit.promote_entity_to_user(element)?;
                edit.promote_entity_to_user(content)?;
                geometry.origin = Origin::User;
                edit.set(element, &geometry)?;
                edit.set(element, &layout)?;
            }
            Ok(())
        })?;
        self.commit(patch).await
    }

    pub(crate) async fn apply_raster_stroke(
        &mut self,
        page: EntityId,
        layer: Option<EntityId>,
        mode: RasterStrokeMode,
        color: [u8; 4],
        diameter: f32,
        points: Vec<ScenePoint>,
    ) -> Result<(Commit, EntityId)> {
        if !diameter.is_finite() || diameter <= 0.0 || points.is_empty() {
            bail!("a raster stroke requires a positive diameter and at least one point");
        }
        if points
            .iter()
            .any(|point| !point.x.is_finite() || !point.y.is_finite())
        {
            bail!("raster stroke points must be finite");
        }
        let snapshot = self.snapshot();
        let page_value = snapshot.page(page)?.page()?;
        let width = page_value.width.round() as u32;
        let height = page_value.height.round() as u32;
        if width == 0 || height == 0 {
            bail!("page dimensions must be positive");
        }
        let mut raster_layer = None;
        let mut promote_layer = false;
        let mut image = if let Some(layer) = layer {
            if snapshot.parent(layer)? != Some(page) {
                bail!("the raster target must be a layer on the active page");
            }
            let target = snapshot
                .component::<SceneRasterLayer>(layer)?
                .context("the raster target must be a layer on the active page")?;
            promote_layer = target.origin != Origin::User
                || snapshot
                    .component::<EntityOrigin>(layer)?
                    .is_some_and(|origin| origin.origin != Origin::User);
            raster_layer = Some(target);
            match snapshot.asset(layer, &AssetRole::new("source")?)? {
                Some(asset) => {
                    let bytes = snapshot.read_blob(asset.blob).await?;
                    image::load_from_memory(&bytes)?.to_rgba8()
                }
                None => RgbaImage::new(width, height),
            }
        } else {
            if mode == RasterStrokeMode::Erase {
                bail!("eraser requires a raster layer target");
            }
            RgbaImage::new(width, height)
        };
        if image.dimensions() != (width, height) {
            bail!("raster layer dimensions must match the page");
        }
        rasterize_stroke(&mut image, mode, color, diameter, &points);
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image).write_to(&mut bytes, ImageFormat::Png)?;
        let source = AssetRole::new("source")?;
        let name = format!(
            "Paint {}",
            snapshot
                .children(page)?
                .filter(|entity| {
                    snapshot
                        .component::<SceneRasterLayer>(*entity)
                        .ok()
                        .flatten()
                        .is_some_and(|layer| layer.kind == RasterLayerKind::Paint)
                })
                .count()
                + 1
        );
        let mut committed_layer = layer;
        let patch = snapshot.patch(|edit| {
            let layer = if let Some(layer) = committed_layer {
                if promote_layer {
                    edit.promote_entity_to_user(layer)?;
                    let raster = raster_layer
                        .as_mut()
                        .expect("an existing raster target has a layer component");
                    raster.origin = Origin::User;
                    edit.set(layer, raster)?;
                }
                layer
            } else {
                let at = snapshot
                    .page(page)?
                    .text_group()?
                    .map_or(At::End, |group| At::Before(group.id()));
                let layer = edit.add_entity(page, at)?;
                edit.set(
                    layer,
                    &SceneRasterLayer {
                        origin: Origin::User,
                        name,
                        kind: RasterLayerKind::Paint,
                    },
                )?;
                committed_layer = Some(layer);
                layer
            };
            edit.set_asset(
                layer,
                &source,
                AssetInput::new(
                    bytes.into_inner(),
                    "image/png",
                    AssetMetadata {
                        width: Some(width),
                        height: Some(height),
                        attributes: Default::default(),
                    },
                ),
            )
        })?;
        Ok((
            self.commit(patch).await?,
            committed_layer.expect("raster layer was selected or added while building the patch"),
        ))
    }

    pub(crate) async fn undo(&mut self) -> Result<Commit> {
        let revisions = self.undo.pop().ok_or_else(|| anyhow!("nothing to undo"))?;
        let commit = match self.session.undo_many(revisions.iter().copied()).await {
            Ok(commit) => commit,
            Err(error) => {
                self.undo.push(revisions);
                return Err(error.into());
            }
        };
        self.redo.push(vec![commit.revision]);
        Ok(commit)
    }

    pub(crate) async fn redo(&mut self) -> Result<Commit> {
        let revisions = self.redo.pop().ok_or_else(|| anyhow!("nothing to redo"))?;
        let commit = match self.session.undo_many(revisions.iter().copied()).await {
            Ok(commit) => commit,
            Err(error) => {
                self.redo.push(revisions);
                return Err(error.into());
            }
        };
        self.undo.push(vec![commit.revision]);
        Ok(commit)
    }

    pub(crate) fn record(&mut self, revisions: Vec<Revision>) {
        if !revisions.is_empty() {
            self.undo.push(revisions);
            self.redo.clear();
        }
    }

    pub(crate) fn record_commit(&mut self, commit: &Commit) {
        if commit.changes.to != commit.changes.from {
            self.record(vec![commit.revision]);
        }
    }

    pub(crate) async fn commit_rebased(
        &mut self,
        patch: koharu_scene::Patch,
    ) -> Result<Option<Commit>> {
        let current = self.snapshot();
        let patch = match patch.rebase_on(&current) {
            Ok(patch) => patch,
            Err(
                error @ (koharu_scene::Error::PatchConflict(_)
                | koharu_scene::Error::EntityNotFound(_)
                | koharu_scene::Error::RelationNotFound(_)),
            ) => {
                tracing::debug!(%error, "pipeline output was superseded by a document edit");
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        if patch.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.commit(patch).await?))
    }

    async fn commit(&mut self, patch: koharu_scene::Patch) -> Result<Commit> {
        Ok(self.session.commit(patch).await?)
    }

    pub(crate) fn page(snapshot: &Snapshot, page: EntityId) -> Result<Page> {
        let value = snapshot.page(page)?.page()?;
        let source = Self::asset_id(snapshot, page, "source")?;
        let mut layers = Vec::new();
        Self::collect_layer_views(snapshot, page, &mut layers)?;
        if let Some(image) = source.clone() {
            layers.insert(
                0,
                Layer::Artwork {
                    id: page,
                    parent: None,
                    geometry: Geometry {
                        points: vec![
                            Point { x: 0.0, y: 0.0 },
                            Point {
                                x: value.width,
                                y: 0.0,
                            },
                            Point {
                                x: value.width,
                                y: value.height,
                            },
                            Point {
                                x: 0.0,
                                y: value.height,
                            },
                        ],
                    },
                    visibility: LayerVisibility {
                        visible: true,
                        opacity: 1.0,
                    },
                    image,
                },
            );
        }
        let regions = snapshot
            .descendants(page)?
            .map(|entity| {
                let id = entity.id();
                snapshot
                    .component::<SceneRegion>(id)?
                    .map(|region| Self::analysis_region(snapshot, id, region))
                    .transpose()
            })
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();
        Ok(Page {
            id: page,
            label: value.label,
            size: PageSize {
                width: value.width,
                height: value.height,
            },
            layers,
            regions,
        })
    }

    fn layer_view(snapshot: &Snapshot, layer: EntityId) -> Result<Layer> {
        let parent = snapshot.parent(layer)?;
        let visibility = Self::layer_visibility(snapshot, layer)?;
        if let Some(group) = snapshot.component::<SceneGroup>(layer)? {
            return Ok(Layer::Group {
                id: layer,
                parent,
                visibility,
                name: group.name,
                role: snapshot
                    .component::<SceneTextGroup>(layer)?
                    .map(|_| GroupRole::Text),
            });
        }
        if let Some(layout) = snapshot.component::<SceneTextLayout>(layer)? {
            let text_layer = snapshot.text_layer(layer)?;
            let content = text_layer.content()?;
            let source = content.source()?.map(|source| SourceText {
                text: source.text.value,
                language: source.language.map(|language| language.to_string()),
            });
            let translation = content.translation()?.map(|translation| Translation {
                text: translation.text.value,
                language: translation.language.map(|language| language.to_string()),
            });
            let role = content.role()?.map(|role| role.role);
            let source_region = content.source_region()?.map(|region| region.id());
            let automatic_region = text_layer.automatic_target()?.map(|region| region.id());
            let typography = text_layer.typography()?.map(Self::typography_view);
            return Ok(Layer::Text {
                id: layer,
                parent,
                geometry: snapshot
                    .component::<SceneGeometry>(layer)?
                    .map(Self::geometry_view),
                angle_degrees: layout.angle_degrees,
                visibility,
                content: Box::new(TextContent {
                    id: content.id(),
                    source,
                    translation,
                    role,
                    source_region,
                }),
                typography,
                layout: layout.kind,
                automatic_region,
            });
        }
        if let Some(raster) = snapshot.component::<SceneRasterLayer>(layer)? {
            return Ok(Layer::Raster {
                id: layer,
                parent,
                visibility,
                image: Self::asset_id(snapshot, layer, "source")?,
                name: raster.name,
                kind: raster.kind,
            });
        }
        let geometry = snapshot
            .component::<SceneGeometry>(layer)?
            .map(Self::geometry_view)
            .context("image layer has no geometry")?;
        let image = Self::asset_id(snapshot, layer, "source")?
            .context("image layer has no source asset")?;
        Ok(Layer::Image {
            id: layer,
            parent,
            geometry,
            visibility,
            image,
        })
    }

    fn analysis_region(
        snapshot: &Snapshot,
        id: EntityId,
        region: SceneRegion,
    ) -> Result<AnalysisRegion> {
        let geometry = snapshot
            .component::<SceneGeometry>(id)?
            .map(Self::geometry_view)
            .context("analysis region has no geometry")?;
        Ok(AnalysisRegion {
            id,
            parent: snapshot.parent(id)?,
            geometry,
            kind: region.kind.as_str().to_owned(),
            label: region.label,
        })
    }

    fn layer_visibility(snapshot: &Snapshot, layer: EntityId) -> Result<LayerVisibility> {
        Ok(snapshot.component::<SceneVisibility>(layer)?.map_or(
            LayerVisibility {
                visible: true,
                opacity: 1.0,
            },
            |visibility| LayerVisibility {
                visible: visibility.visible,
                opacity: visibility.opacity,
            },
        ))
    }

    fn typography_view(typography: SceneTypography) -> Typography {
        let writing_mode = if matches!(&typography.origin, Origin::User) {
            typography.writing_mode
        } else {
            None
        };
        Typography {
            preferred_font: typography.preferred_font,
            font_weight: typography.font_weight,
            font_style: typography.font_style,
            size: typography.size,
            auto_fit: typography.auto_fit,
            color: typography.color,
            stroke_color: typography.stroke_color,
            stroke_width: typography.stroke_width,
            alignment: typography.alignment,
            writing_mode,
        }
    }

    fn geometry_view(geometry: SceneGeometry) -> Geometry {
        Geometry {
            points: geometry
                .points
                .into_iter()
                .map(|point| Point {
                    x: point.x,
                    y: point.y,
                })
                .collect(),
        }
    }

    fn asset_id(snapshot: &Snapshot, entity: EntityId, role: &str) -> Result<Option<String>> {
        Ok(snapshot
            .asset(entity, &AssetRole::new(role)?)?
            .map(|asset| asset.blob.to_string()))
    }

    fn is_layer(snapshot: &Snapshot, entity: EntityId) -> Result<bool> {
        Ok(snapshot.component::<SceneGroup>(entity)?.is_some()
            || snapshot.component::<SceneTextLayout>(entity)?.is_some()
            || snapshot.component::<SceneRasterLayer>(entity)?.is_some()
            || (snapshot.component::<SceneGeometry>(entity)?.is_some()
                && snapshot
                    .asset(entity, &AssetRole::new("source")?)?
                    .is_some()))
    }

    fn is_content_layer(snapshot: &Snapshot, entity: EntityId) -> Result<bool> {
        Ok(
            snapshot.component::<SceneGroup>(entity)?.is_none()
                && Self::is_layer(snapshot, entity)?,
        )
    }

    fn collect_layer_views(
        snapshot: &Snapshot,
        parent: EntityId,
        layers: &mut Vec<Layer>,
    ) -> Result<()> {
        for child in snapshot.children(parent)? {
            if !Self::is_layer(snapshot, child)? {
                continue;
            }
            let group = snapshot.component::<SceneGroup>(child)?.is_some();
            layers.push(Self::layer_view(snapshot, child)?);
            if group {
                Self::collect_layer_views(snapshot, child, layers)?;
            }
        }
        Ok(())
    }

    fn text_content(snapshot: &Snapshot, layer: EntityId) -> Result<EntityId> {
        snapshot
            .text_layer(layer)?
            .content()
            .map(|content| content.id())
            .map_err(Into::into)
    }

    fn placement(siblings: &[EntityId], moving: EntityId, index: usize) -> At {
        siblings
            .iter()
            .copied()
            .filter(|entity| *entity != moving)
            .nth(index)
            .map_or(At::End, At::Before)
    }

    fn unique_roots(snapshot: &Snapshot, entities: Vec<EntityId>) -> Result<Vec<EntityId>> {
        let selected = entities.into_iter().collect::<HashSet<_>>();
        let mut roots = Vec::new();
        for entity in selected.iter().copied() {
            let mut parent = snapshot.parent(entity)?;
            let mut nested = false;
            while let Some(value) = parent {
                if selected.contains(&value) {
                    nested = true;
                    break;
                }
                parent = snapshot.parent(value)?;
            }
            if !nested {
                roots.push(entity);
            }
        }
        roots.sort_unstable();
        Ok(roots)
    }

    fn geometry_from_frame(frame: Frame) -> Result<SceneGeometry> {
        if !frame.x.is_finite()
            || !frame.y.is_finite()
            || !frame.width.is_finite()
            || !frame.height.is_finite()
            || !frame.angle_degrees.is_finite()
            || frame.width <= 0.0
            || frame.height <= 0.0
        {
            bail!("frame must contain finite coordinates and positive dimensions");
        }
        let center_x = f64::from(frame.x + frame.width * 0.5);
        let center_y = f64::from(frame.y + frame.height * 0.5);
        let half_width = f64::from(frame.width) * 0.5;
        let half_height = f64::from(frame.height) * 0.5;
        let (sin, cos) = f64::from(frame.angle_degrees).to_radians().sin_cos();
        Ok(SceneGeometry {
            origin: Origin::User,
            points: [
                (-half_width, -half_height),
                (half_width, -half_height),
                (half_width, half_height),
                (-half_width, half_height),
            ]
            .map(|(x, y)| ScenePoint {
                x: center_x + x * cos - y * sin,
                y: center_y + x * sin + y * cos,
            })
            .into(),
        })
    }
}

/// 一段目录名的合法性。
///
/// 漫画目录名与章目录名用的是同一套规则，所以规则只有这一处。它们各自构成路径的一段，因此禁掉
/// 分隔符：路径不靠拼接出来的名字解析，而是靠名字本身就是一段干净的目录名。
pub(crate) fn validate_directory_name(name: &str) -> Result<String> {
    let name = name.trim();
    if name.is_empty() {
        bail!("a directory name cannot be empty");
    }
    if name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        bail!("{name:?} contains characters that cannot be used in a directory name");
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
    {
        bail!("{name:?} is reserved by Windows");
    }
    Ok(name.to_owned())
}

fn rasterize_stroke(
    image: &mut RgbaImage,
    mode: RasterStrokeMode,
    color: [u8; 4],
    diameter: f32,
    points: &[ScenePoint],
) {
    let radius = f64::from(diameter) * 0.5;
    for (start, end) in points
        .iter()
        .zip(points.iter().skip(1))
        .chain(points.last().map(|point| (point, point)))
    {
        let left = (start.x.min(end.x) - radius - 0.5).floor().max(0.0) as u32;
        let top = (start.y.min(end.y) - radius - 0.5).floor().max(0.0) as u32;
        let right = (start.x.max(end.x) + radius + 0.5)
            .ceil()
            .min(f64::from(image.width())) as u32;
        let bottom = (start.y.max(end.y) + radius + 0.5)
            .ceil()
            .min(f64::from(image.height())) as u32;
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        let length_squared = dx * dx + dy * dy;
        for y in top..bottom {
            for x in left..right {
                let px = f64::from(x) + 0.5;
                let py = f64::from(y) + 0.5;
                let t = if length_squared == 0.0 {
                    0.0
                } else {
                    (((px - start.x) * dx + (py - start.y) * dy) / length_squared).clamp(0.0, 1.0)
                };
                let distance =
                    ((px - (start.x + t * dx)).powi(2) + (py - (start.y + t * dy)).powi(2)).sqrt();
                let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0) as f32;
                if coverage == 0.0 {
                    continue;
                }
                let pixel = image.get_pixel_mut(x, y);
                match mode {
                    RasterStrokeMode::Paint => {
                        let source_alpha = f32::from(color[3]) / 255.0 * coverage;
                        let destination_alpha = f32::from(pixel[3]) / 255.0;
                        let output_alpha = source_alpha + destination_alpha * (1.0 - source_alpha);
                        if output_alpha > 0.0 {
                            for channel in 0..3 {
                                let source = f32::from(color[channel]) / 255.0;
                                let destination = f32::from(pixel[channel]) / 255.0;
                                pixel[channel] = (((source * source_alpha
                                    + destination * destination_alpha * (1.0 - source_alpha))
                                    / output_alpha)
                                    * 255.0)
                                    .round() as u8;
                            }
                        }
                        pixel[3] = (output_alpha * 255.0).round() as u8;
                    }
                    RasterStrokeMode::Erase => {
                        pixel[3] = (f32::from(pixel[3]) * (1.0 - coverage)).round() as u8;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use koharu_scene::{Generation, ProducerId, WritingMode};

    use super::*;

    #[test]
    fn only_user_authored_direction_is_projected_as_an_override() {
        let mut typography = SceneTypography {
            origin: Origin::Generated(Generation::new(
                ProducerId::new("dev.koharu.pipeline.detection").expect("valid producer"),
            )),
            preferred_font: None,
            font_weight: None,
            font_style: None,
            size: None,
            auto_fit: true,
            color: None,
            stroke_color: None,
            stroke_width: None,
            alignment: None,
            writing_mode: Some(WritingMode::Vertical),
            extensions: Default::default(),
        };

        assert_eq!(
            Project::typography_view(typography.clone()).writing_mode,
            None
        );
        typography.origin = Origin::User;
        assert_eq!(
            Project::typography_view(typography).writing_mode,
            Some(WritingMode::Vertical)
        );
    }

    #[tokio::test]
    async fn pipeline_commit_rebases_or_yields_to_the_manual_edit() {
        let mut session = Session::memory().await.unwrap();
        let mut setup = session.snapshot().edit();
        let pipeline_page = setup
            .add_page(PageDraft::new("pipeline", 100.0, 100.0), At::End)
            .unwrap();
        let manual_page = setup
            .add_page(PageDraft::new("manual", 100.0, 100.0), At::End)
            .unwrap();
        session.commit(setup.finish().unwrap()).await.unwrap();
        let mut project = Project::new(
            session,
            ChapterRef { series: "series".to_owned(), chapter: "Chapter 1".to_owned() },
            "series - Chapter 1".to_owned(),
        );

        let base = project.snapshot();
        let pipeline = base
            .patch(|edit| {
                edit.set_page(
                    pipeline_page,
                    PageDraft::new("pipeline result", 100.0, 100.0),
                )
            })
            .unwrap();
        let manual = base
            .patch(|edit| edit.set_page(manual_page, PageDraft::new("manual edit", 100.0, 100.0)))
            .unwrap();
        project.commit(manual).await.unwrap();

        let commit = project.commit_rebased(pipeline).await.unwrap().unwrap();
        assert_eq!(
            commit
                .snapshot
                .page(pipeline_page)
                .unwrap()
                .page()
                .unwrap()
                .label,
            "pipeline result"
        );
        assert_eq!(
            commit
                .snapshot
                .page(manual_page)
                .unwrap()
                .page()
                .unwrap()
                .label,
            "manual edit"
        );

        let base = project.snapshot();
        let pipeline = base
            .patch(|edit| {
                edit.set_page(
                    pipeline_page,
                    PageDraft::new("stale pipeline", 100.0, 100.0),
                )
            })
            .unwrap();
        let manual = base
            .patch(|edit| {
                edit.set_page(pipeline_page, PageDraft::new("latest manual", 100.0, 100.0))
            })
            .unwrap();
        project.commit(manual).await.unwrap();

        assert!(project.commit_rebased(pipeline).await.unwrap().is_none());
        assert_eq!(
            project
                .snapshot()
                .page(pipeline_page)
                .unwrap()
                .page()
                .unwrap()
                .label,
            "latest manual"
        );
    }

    #[test]
    fn raster_strokes_are_continuous_and_erasable() {
        let mut image = RgbaImage::new(32, 16);
        let points = [
            ScenePoint { x: 4.0, y: 8.0 },
            ScenePoint { x: 28.0, y: 8.0 },
        ];
        rasterize_stroke(
            &mut image,
            RasterStrokeMode::Paint,
            [210, 40, 20, 255],
            5.0,
            &points,
        );
        for x in 4..28 {
            assert_eq!(image.get_pixel(x, 8).0, [210, 40, 20, 255]);
        }

        rasterize_stroke(
            &mut image,
            RasterStrokeMode::Erase,
            [0, 0, 0, 0],
            5.0,
            &[ScenePoint { x: 16.0, y: 8.0 }],
        );
        assert_eq!(image.get_pixel(16, 8)[3], 0);
        assert_eq!(image.get_pixel(4, 8)[3], 255);

        let mut white = RgbaImage::new(4, 4);
        rasterize_stroke(
            &mut white,
            RasterStrokeMode::Paint,
            [255, 255, 255, 255],
            3.0,
            &[ScenePoint { x: 2.0, y: 2.0 }],
        );
        assert_eq!(white.get_pixel(2, 2).0, [255, 255, 255, 255]);
    }
}
