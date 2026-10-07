use std::{
    borrow::Cow,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use anyhow::{Context as _, Result, bail};
use image::{DynamicImage, ImageFormat, ImageReader, ImageBuffer, Rgb};
use koharu_ml::webtoon::{RowStat, SliceParams, plan_slices, row_profile};
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, Commit, PageDraft,
};
use rayon::prelude::*;
use strum::{EnumIter, EnumMessage, EnumString};
use walkdir::WalkDir;

use super::project::Project;
use super::series::AdBands;

mod pdf;
mod rar;
mod zip;

/// JPEG quality for an encoded band.
///
/// A band is re-encoded rather than copied out of the source because the source pixels cannot be
/// handed over without re-running a codec over them anyway; 92 keeps grain and thin strokes, which
/// is what the detector reads, while staying far smaller than PNG.
const BAND_JPEG_QUALITY: u8 = 92;

#[derive(Clone, Copy, EnumIter, EnumMessage, EnumString)]
#[strum(ascii_case_insensitive)]
pub(super) enum Format {
    #[strum(
        serialize = "png",
        serialize = "jpg",
        serialize = "jpeg",
        serialize = "webp"
    )]
    Raster,
    #[strum(serialize = "cbz", serialize = "zip")]
    Zip,
    #[strum(serialize = "rar")]
    Rar,
    #[strum(serialize = "pdf")]
    Pdf,
}

/// Whether tall images are cut into pages on the way in.
///
/// 只有 `Auto` 一种：强制切页原本是编辑器里那个条漫导入入口的选项，而导入现在只从漫画柜走，
/// 那里由用户在导入前选定章节形态，不需要再为切页单独做一次决定（`docs/series-management-design.md` §1）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Slicing {
    /// Cut an image only when its geometry says it is a webtoon.
    Auto,
}

impl Slicing {
    /// Planner parameters for this mode.
    fn params(self) -> SliceParams {
        match self {
            Self::Auto => SliceParams::default(),
        }
    }
}

#[derive(Debug)]
pub(super) struct EncodedPage {
    pub(super) name: String,
    pub(super) bytes: Vec<u8>,
}

pub(super) struct Page {
    pub(super) name: String,
    pub(super) bytes: Arc<[u8]>,
    pub(super) format: ImageFormat,
    pub(super) width: u32,
    pub(super) height: u32,
}

/// One imported source image, already reduced to the pages the project should hold.
pub(super) enum Imported {
    /// An image that is already one readable page.
    Page(Page),
    /// A tall image divided into pages at import time.
    Strip {
        /// The uncut image, retained so the strip can be cut again with other boundaries.
        source: Arc<[u8]>,
        /// The media type of the uncut image, needed to store it as a project's asset.
        format: ImageFormat,
        width: u32,
        height: u32,
        bands: Vec<Band>,
    },
}

/// One page cut out of a tall image.
pub(super) struct Band {
    /// Distance from the top of the uncut image, recorded as the page's provenance.
    pub(super) y_offset: u32,
    pub(super) page: Page,
}

/// 一次条漫导入的产物，以及广告带对这一批源图做了什么。
///
/// 报告与页面一起返回，而不是放在旁边：调用方不可能把页面提交进去却把「这批图被设置削掉了什么」
/// 弄丢。
pub(super) struct WebtoonImport {
    pub(super) imported: Vec<Imported>,
    pub(super) ad_bands: AdBandReport,
}

/// 一批源图上广告带的执行结果。
///
/// 一批就是一章，所以没有任何单张图能自己决定命运：广告带放不下的那张在这里报告，而不是让整批
/// 导入失败。
#[derive(Debug, Default)]
pub(super) struct AdBandReport {
    /// 裁掉广告带后切成多页的图。
    trimmed: u32,
    /// 没有应用广告带的图，按导入顺序。它们仍然整张进项目，除了 [`AdBandSkip::Covered`]——
    /// 那是唯一一种「什么都不剩、整张不进项目」的理由。
    skipped: Vec<SkippedAdBands>,
}

impl AdBandReport {
    /// 多少张图在裁掉广告带后被切了页。
    pub(super) fn trimmed(&self) -> u32 {
        self.trimmed
    }

    /// 多少张图没有应用广告带，按导入顺序。
    pub(super) fn skipped(&self) -> &[SkippedAdBands] {
        &self.skipped
    }
}

/// 一张没有应用广告带的源图。
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SkippedAdBands {
    /// 这张图导入时用的名字，用户靠它找回这张图。
    pub(super) name: String,
    pub(super) reason: AdBandSkip,
}

/// 一张图没有被应用广告带的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AdBandSkip {
    /// 两条广告带覆盖了整张图，说明这个设置对这份文件是错的，而不是不方便。
    Covered,
    /// 两条广告带之间剩下的高度小于规划器允许的最短页，这张图整张进项目，而不是被切成碎片。
    TooShort,
    /// 剩下的部分本身已经是一页，切它只会把读者能读的一页切碎。
    NotSliceable,
}

fn decode(path: &Path, source: EncodedPage) -> Result<Page> {
    let EncodedPage { name, bytes } = source;
    let format = image::guess_format(&bytes).with_context(|| {
        format!(
            "failed to identify imported image {} ({name})",
            path.display()
        )
    })?;
    let (width, height) = ImageReader::with_format(Cursor::new(bytes.as_slice()), format)
        .into_dimensions()
        .with_context(|| {
            format!(
                "failed to read dimensions of imported image {} ({name})",
                path.display()
            )
        })?;
    Ok(Page {
        name,
        bytes: Arc::<[u8]>::from(bytes),
        format,
        width,
        height,
    })
}

/// Reads every supported page source, leaving each image whole.
///
/// A tall image becomes exactly one page here no matter how tall it is. Dividing long strips is
/// the webtoon importer's job, and it is a separate entry point so that this path stays the
/// upstream command's behaviour.
pub(super) fn import(paths: Vec<PathBuf>) -> Result<Vec<Imported>> {
    Ok(read(paths)?.into_iter().map(Imported::Page).collect())
}

/// Reads every supported page source and divides the tall ones into pages, reporting what the
/// series' ad bands did on the way.
///
/// The geometry gate lives in the planner, so forcing a cut relaxes the gate rather than adding a
/// second cutting path: an ordinary page still produces exactly one page.
pub(super) fn import_webtoon(
    paths: Vec<PathBuf>,
    slicing: Slicing,
    ad: AdBands,
) -> Result<WebtoonImport> {
    let (imported, ad_bands) = cut(read(paths)?, slicing, ad);
    tracing::info!(
        trimmed = ad_bands.trimmed(),
        without_bands = ad_bands.skipped().len(),
        "webtoon import finished"
    );
    Ok(WebtoonImport {
        imported,
        ad_bands,
    })
}

/// Collects every importable file under a directory, one level of recursion deep.
///
/// The scan is recursive but never follows symlinks, and an entry that cannot be read is skipped
/// rather than failing the whole selection, so one unreadable file never costs the user the rest
/// of a chapter. Unsupported files are dropped silently because a chapter directory usually also
/// holds artwork, notes and archives that are not pages.
pub(super) fn collect_importable(directory: &Path) -> Result<Vec<PathBuf>> {
    Ok(WalkDir::new(directory)
        .follow_links(false)
        .into_iter()
        .filter_map(|entry| match entry {
            Ok(entry) if entry.file_type().is_file() => Some(entry.into_path()),
            Ok(_) => None,
            Err(error) => {
                tracing::warn!(%error, "could not inspect an import directory entry");
                None
            }
        })
        .filter(|path| {
            path.extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.parse::<Format>().is_ok())
        })
        .collect())
}

/// Writes an import result into a project and commits it.
///
/// The three import paths share this: a plain import, a webtoon import, and the series layer
/// importing a whole shelf. A strip becomes pages through the extension layer's custom component,
/// so this only has to understand the one intermediate shape.
pub(super) async fn apply(project: &mut Project, imported: Vec<Imported>) -> Result<Commit> {
    let role = AssetRole::new("source")?;
    let patch = project.snapshot().patch(|edit| {
        for entry in imported {
            match entry {
                Imported::Page(page) => {
                    let id = edit.add_page(
                        PageDraft::new(
                            page.name,
                            f64::from(page.width),
                            f64::from(page.height),
                        ),
                        At::End,
                    )?;
                    edit.set_asset(
                        id,
                        &role,
                        AssetInput::new(
                            page.bytes,
                            page.format.to_mime_type(),
                            AssetMetadata {
                                width: Some(page.width),
                                height: Some(page.height),
                                attributes: Default::default(),
                            },
                        ),
                    )?;
                }
                Imported::Strip {
                    source,
                    format,
                    width,
                    height,
                    bands,
                } => {
                    crate::webtoon::add_strip(
                        edit,
                        crate::webtoon::StripInput {
                            bytes: source,
                            width: f64::from(width),
                            height: f64::from(height),
                            media_type: format.to_mime_type().to_owned(),
                            bands: bands
                                .into_iter()
                                .map(|band| {
                                    let page = band.page;
                                    crate::webtoon::BandInput {
                                        label: page.name,
                                        y_offset: f64::from(band.y_offset),
                                        height: f64::from(page.height),
                                        bytes: page.bytes,
                                        media_type: page.format.to_mime_type().to_owned(),
                                    }
                                })
                                .collect(),
                        },
                        At::End,
                    )?;
                }
            }
        }
        Ok(())
    })?;
    Ok(project.session.commit(patch).await?)
}

/// Reads and sorts every supported page source without deciding how tall images are divided.
fn read(mut paths: Vec<PathBuf>) -> Result<Vec<Page>> {
    alphanumeric_sort::sort_slice_by_os_str_key(&mut paths, |path| {
        path.file_name().unwrap_or_else(|| path.as_os_str())
    });
    let mut groups = paths
        .into_par_iter()
        .map(|path| -> Result<Vec<Page>> {
            let extension = path
                .extension()
                .and_then(|extension| extension.to_str())
                .and_then(|extension| extension.parse::<Format>().ok());
            let encoded = match extension {
                Some(Format::Raster) => vec![EncodedPage {
                    name: path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "page".to_owned()),
                    bytes: fs::read(&path)
                        .with_context(|| format!("failed to read {}", path.display()))?,
                }],
                Some(Format::Zip) => zip::extract(&path)?,
                Some(Format::Rar) => rar::extract(&path)?,
                Some(Format::Pdf) => pdf::render(&path)?,
                None => bail!("unsupported page import path {}", path.display()),
            };
            encoded
                .into_iter()
                .map(|source| decode(&path, source))
                .collect()
        })
        .collect::<Result<Vec<_>>>()?;
    let page_count = groups.iter().map(Vec::len).sum();
    let mut pages = Vec::with_capacity(page_count);
    for group in &mut groups {
        pages.append(group);
    }
    Ok(pages)
}

/// Divides webtoon strips into pages and leaves every other image whole.
///
/// Whether a strip is cut is the planner's decision: `plan_slices` returns `None` for
/// anything it considers readable as one page, so the geometry rule lives in exactly one
/// place. The local pre-check exists only to skip the full decode of images that cannot
/// possibly be cut, and it reads the planner's own thresholds instead of restating them.
///
/// The ad bands are resolved per image before any of them is processed, because they describe the two
/// ends of a chapter rather than the two ends of every file in it.
fn cut(pages: Vec<Page>, slicing: Slicing, ad: AdBands) -> (Vec<Imported>, AdBandReport) {
    let params = slicing.params();
    let last = pages.len().saturating_sub(1);
    let outcomes = pages
        .into_par_iter()
        .enumerate()
        .map(|(index, page)| {
            let bands = ad.for_page(index, last);
            (bands, cut_one(page, &params, bands))
        })
        .collect::<Vec<_>>();
    let mut imported = Vec::with_capacity(outcomes.len());
    let mut report = AdBandReport::default();
    for (bands, outcome) in outcomes {
        match outcome {
            CutOutcome::Imported(entry) => {
                // 切片只会在规划器真的分了页时产生，所以「这张图背着自己的广告带且产出 Strip」正好就是
                // 被裁过的那批图。
                if !bands.is_empty() && matches!(entry, Imported::Strip { .. }) {
                    report.trimmed += 1;
                }
                imported.push(entry);
            }
            CutOutcome::ImportedWhole { entry, name, reason } => {
                // 报告与导入同时发生：静默地导入会让用户以为设置生效了。
                report.skipped.push(SkippedAdBands { name, reason });
                imported.push(entry);
            }
            CutOutcome::Dropped { name, reason } => {
                report.skipped.push(SkippedAdBands { name, reason });
            }
        }
    }
    (imported, report)
}

/// 一张源图的处理结果，在整批组装之前。
enum CutOutcome {
    /// 这张图，已经裁成项目该持有的形态。
    Imported(Imported),
    /// 广告带放不下这张图，而它仍然有内容可导入：整张进项目，同时说明没裁。
    ImportedWhole {
        entry: Imported,
        name: String,
        reason: AdBandSkip,
    },
    /// 广告带吃掉了整张图，没有任何内容可导入。
    Dropped { name: String, reason: AdBandSkip },
}

/// 裁一张源图，`ad` 是这张图自己承担的那一端广告带（见 [`AdBands::for_page`]）。
fn cut_one(page: Page, params: &SliceParams, ad: AdBands) -> CutOutcome {
    // 规划器会整张留下的图也整张留下：削它等于把一个本来就能读的一页上面的广告去掉。
    if !may_be_a_webtoon(page.width, page.height, params) {
        return CutOutcome::Imported(Imported::Page(page));
    }
    let (head, tail) = (ad.head, ad.tail);
    // 显式检查：广告带是用户输入的，`head + tail` 越过图底必须读成设置错误，而不是回绕成一个看起来
    // 合理的中段。
    let Some(middle) = page
        .height
        .checked_sub(head)
        .and_then(|rest| rest.checked_sub(tail))
        .filter(|middle| *middle > 0)
    else {
        tracing::warn!(
            page = %page.name,
            height = page.height,
            head,
            tail,
            "the ad bands cover the whole image"
        );
        return CutOutcome::Dropped {
            name: page.name,
            reason: AdBandSkip::Covered,
        };
    };
    // 一批就是一章，所以广告带会剩得太短、切不出页的那张图整张进项目，而不是被丢掉：一张尺寸特殊的
    // 文件不该让用户失去整章。
    if middle < params.min_height {
        return keep_whole(page, ad, AdBandSkip::TooShort);
    }
    let started = Instant::now();
    let image = match image::load_from_memory_with_format(&page.bytes, page.format) {
        Ok(image) => image,
        Err(error) => {
            // Dimensions were already read from the header, so a decode failure here means a
            // truncated or corrupt body. Importing the original keeps the user's file visible
            // instead of dropping it over a failed optimisation.
            tracing::warn!(%error, page = %page.name, "could not decode an imported image for slicing");
            return CutOutcome::Imported(Imported::Page(page));
        }
    };
    let decode = started.elapsed();
    // 灰度化与行剖面各自都是一整趟 O(宽×高) 的遍历，合在一起计时就没法判断该优化哪一个，所以
    // 两者在这里就被拆成两段。它们仍然是同一趟工作的一部分，顺序也没变。
    let started = Instant::now();
    let luma = image.to_luma8();
    let to_luma = started.elapsed();
    let started = Instant::now();
    let profile = row_profile(&luma);
    let profiling = started.elapsed();
    tracing::info!(
        page = %page.name,
        width = page.width,
        height = page.height,
        ?decode,
        ?to_luma,
        ?profiling,
        "read an imported image for slicing"
    );
    // 广告带永远不从像素里裁掉：剖面被裁成中段，切点读像素时再加上 `head`，因此每一条切片仍然逐
    // 像素来自原图。
    let Some(rows) = middle_rows(&profile, head, head + middle) else {
        // 文件头给了一个高度，解码出来是另一个，所以广告带指向的行不是存在的行。
        tracing::warn!(
            page = %page.name,
            height = page.height,
            "an imported image does not match its reported height"
        );
        return CutOutcome::Imported(Imported::Page(page));
    };
    let Some(plan) = plan_slices(page.width, middle, &rows, params) else {
        return keep_whole(page, ad, AdBandSkip::NotSliceable);
    };
    // A plan that cuts nothing describes the image that is already there. Reporting it as a
    // strip would give an ordinary page a band record claiming it was cut out of itself.
    if plan.page_count() <= 1 {
        return keep_whole(page, ad, AdBandSkip::NotSliceable);
    }
    match band(&page, &image, head, &plan) {
        Ok(bands) => CutOutcome::Imported(Imported::Strip {
            source: page.bytes,
            format: page.format,
            // 条带仍然是未切割的图，因为那才是切片指向的东西。`plan.height` 只是广告带规划所覆盖
            // 的那段中段。
            width: page.width,
            height: page.height,
            bands,
        }),
        Err(error) => {
            tracing::warn!(%error, page = %page.name, "could not cut an imported webtoon strip");
            CutOutcome::Imported(Imported::Page(page))
        }
    }
}

/// 整张进项目，并在设过广告带时说明为什么没裁。
///
/// 「没裁」不是「丢掉」：广告带放不下的那张图仍然要进项目，否则用户会失去整章。只有广告带吃掉了
/// 整张图才什么都不剩，那才是 [`CutOutcome::Dropped`]。
fn keep_whole(page: Page, ad: AdBands, reason: AdBandSkip) -> CutOutcome {
    if ad.is_empty() {
        return CutOutcome::Imported(Imported::Page(page));
    }
    tracing::warn!(page = %page.name, height = page.height, ?reason, "the ad bands were not applied");
    let name = page.name.clone();
    CutOutcome::ImportedWhole {
        entry: Imported::Page(page),
        name,
        reason,
    }
}

/// 规划器眼中的图：两条广告带之间的那些行，行号从**中段顶端**起算而不是从源图顶端。
///
/// `plan_slices` 把 `RowStat::y` 当作切点位置读，所以这些行是**重基**过而不只是截断过的：直接切子
/// 切片会让每一行保留源图自己的行号，于是每个切点都提前 `head` 像素。
fn middle_rows<'a>(profile: &'a [RowStat], head: u32, end: u32) -> Option<Cow<'a, [RowStat]>> {
    let rows = profile.get(head as usize..end as usize)?;
    Some(if head == 0 {
        Cow::Borrowed(rows)
    } else {
        Cow::Owned(
            rows.iter()
                .map(|stat| RowStat {
                    y: stat.y - head,
                    ..*stat
                })
                .collect(),
        )
    })
}

fn may_be_a_webtoon(width: u32, height: u32, params: &SliceParams) -> bool {
    height > params.min_sliceable_height && height as f32 > params.trigger_aspect * width as f32
}

fn band(
    page: &Page,
    image: &DynamicImage,
    head: u32,
    plan: &koharu_ml::webtoon::SlicePlan,
) -> Result<Vec<Band>> {
    // 切片一律是 JPEG，与源格式无关：未切割的源图已经完整存在项目里，切片是派生数据，所以没有
    // 理由为无损源再付一次无损重压缩的代价。
    let format = ImageFormat::Jpeg;
    // 切片之间没有任何依赖，所以整批并行。一张长条切出七八片是常态，单线程跑等于把核扔掉，
    // 而"一章有几张长图"并不是能依赖的前提——一张源图也必须吃满核。
    let started = Instant::now();
    let bands = plan
        .page_ranges()
        .into_par_iter()
        .enumerate()
        .map(|(index, (y_offset, height))| {
            // 规划器的行号从中段顶端起算；裁剪与记录的偏移都是源图行号，所以首条广告带恰好加回一次。
            let y_offset = head + y_offset;
            let cropped = image.crop_imm(0, y_offset, plan.width, height);
            Ok(Band {
                y_offset,
                page: Page {
                    // Strip pages are unnamed parts of one file, so a positional name is the only
                    // label that stays meaningful without inventing chapter numbering.
                    name: format!("{} {:02}", strip_stem(&page.name), index + 1),
                    bytes: encode(&cropped)?,
                    format,
                    width: plan.width,
                    height,
                },
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let elapsed = started.elapsed();
    let encoded: usize = bands.iter().map(|band| band.page.bytes.len()).sum();
    tracing::info!(
        page = %page.name,
        bands = bands.len(),
        ?elapsed,
        encoded_bytes = encoded,
        "cut an imported webtoon strip into pages"
    );
    Ok(bands)
}

fn strip_stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |(stem, _)| stem)
}

/// 切片编码前的 RGB 视图：带透明度的源图合成到白纸上，其余只降位深。
///
/// JPEG 没有 alpha 通道，而丢掉它会让透明区域变成文件里存的那几个 RGB 值——通常是黑的，于是
/// 白纸变黑纸。已经是 RGB 的源图不复制。
fn opaque_rgb(image: &DynamicImage) -> Cow<'_, ImageBuffer<Rgb<u8>, Vec<u8>>> {
    if let Some(rgb) = image.as_rgb8() {
        return Cow::Borrowed(rgb);
    }
    let mut rgba = image.to_rgba8();
    for pixel in rgba.pixels_mut() {
        let alpha = f32::from(pixel.0[3]) / 255.0;
        if alpha < 1.0 {
            for channel in &mut pixel.0[..3] {
                *channel = (f32::from(*channel) * alpha + 255.0 * (1.0 - alpha)).round() as u8;
            }
        }
    }
    // alpha 已经全部是 255，所以这一次降位深只是丢通道，不再有透明要处理。
    Cow::Owned(DynamicImage::ImageRgba8(rgba).to_rgb8())
}

/// 切片的重编码器。
///
/// 只有一种，因为切片只有一种形态：未切割的源图已经完整存在项目里（`PageSlice.source` 钉住它
/// 的 blob），所以切片是派生数据。对已经压缩过的像素再做一次无损重压缩，买到的只是更大的字节
/// 和更长的等待——实测一条 724×15120 的 PNG 长条，切成 7 片：无损 5883 ms / 17.6 MiB，
/// q92 是 1866 ms / 4.8 MiB。
///
/// 92 保住网点与细描边，那些正是检测器要读的东西，同时远小于 PNG。
fn encode(image: &DynamicImage) -> Result<Arc<[u8]>> {
    let mut bytes = Cursor::new(Vec::new());
    let mut jpeg =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, BAND_JPEG_QUALITY);
    let rgb = opaque_rgb(image);
    jpeg.encode_image(&*rgb)?;
    Ok(Arc::from(bytes.into_inner()))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn fixture(name: &str, image: &DynamicImage) -> PathBuf {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("koharu-import-{}-{timestamp}", std::process::id()));
        fs::create_dir_all(&directory).expect("create fixture directory");
        let mut encoded = Cursor::new(Vec::new());
        image
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode fixture");
        let path = directory.join(name);
        fs::write(&path, encoded.get_ref()).expect("write fixture");
        path
    }

    /// A strip of `height` rows separated by white gutters, so the planner has real cut
    /// candidates rather than having to fall back to the flattest row.
    fn strip_image(width: u32, height: u32) -> DynamicImage {
        let mut image =
            image::RgbaImage::from_pixel(width, height, image::Rgba([255, 255, 255, 255]));
        for top in (0..height).step_by(1600) {
            for y in top..(top + 1200).min(height) {
                for x in 0..width {
                    image.put_pixel(x, y, image::Rgba([20, 20, 20, 255]));
                }
            }
        }
        DynamicImage::ImageRgba8(image)
    }

    #[test]
    fn top_level_paths_are_naturally_sorted() {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "koharu-import-order-{}-{timestamp}",
            std::process::id()
        ));
        fs::create_dir(&directory).expect("create fixture directory");
        let image = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut encoded = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut encoded, ImageFormat::Png)
            .expect("encode fixture");
        let paths = ["page10.PNG", "page2.png", "page1.png"].map(|name| directory.join(name));
        for path in &paths {
            fs::write(path, encoded.get_ref()).expect("write fixture");
        }

        let pages = read(paths.into()).expect("read fixtures");
        fs::remove_dir_all(&directory).expect("remove fixture directory");
        assert_eq!(
            pages
                .iter()
                .map(|page| page.name.as_str())
                .collect::<Vec<_>>(),
            ["page1.png", "page2.png", "page10.PNG"]
        );
    }

    /// 某个源图是以什么形态进的导入，按文件名词干查出来。导入按文件名排序，所以是查而不是取下标。
    fn split<'a>(imported: &'a [Imported], stem: &str) -> &'a Imported {
        imported
            .iter()
            .find(|entry| match entry {
                Imported::Page(page) => page.name.starts_with(stem),
                Imported::Strip { bands, .. } => bands
                    .first()
                    .is_some_and(|band| band.page.name.starts_with(stem)),
            })
            .unwrap_or_else(|| panic!("{stem} is missing from the import"))
    }

    #[test]
    fn ordinary_pages_are_never_cut_and_webtoons_always_are() {
        let page = fixture("page.png", &strip_image(800, 1600));
        let strip = fixture("chapter.png", &strip_image(360, 6000));
        let paths = vec![page.clone(), strip.clone()];

        // Imports are ordered by file name, so entries are looked up rather than indexed.
        fn split<'a>(imported: &'a [Imported], stem: &str) -> &'a Imported {
            imported
                .iter()
                .find(|entry| match entry {
                    Imported::Page(page) => page.name.starts_with(stem),
                    Imported::Strip { bands, .. } => bands
                        .first()
                        .is_some_and(|band| band.page.name.starts_with(stem)),
                })
                .unwrap_or_else(|| panic!("{stem} is missing from the import"))
        }

        // The upstream import leaves every image whole, however tall it is: dividing strips is the
        // webtoon importer's job, so the upstream command's behaviour is unchanged.
        let plain = import(paths.clone()).expect("import fixtures");
        let mut heights = plain
            .iter()
            .map(|entry| match entry {
                Imported::Page(page) => page.height,
                Imported::Strip { height, .. } => *height,
            })
            .collect::<Vec<_>>();
        heights.sort_unstable();
        assert_eq!(heights, vec![1600, 6000]);

        let imported = import_webtoon(paths.clone(), Slicing::Auto, AdBands::default())
            .expect("import fixtures");
        assert!(matches!(split(&imported.imported, "page"), Imported::Page(page) if page.height == 1600));
        let Imported::Strip {
            width,
            height,
            bands,
            ..
        } = split(&imported.imported, "chapter")
        else {
            panic!("a 1:16 image must be cut on import");
        };
        assert_eq!((*width, *height), (360, 6000));
        assert!(bands.len() > 1);
        // Bands tile the strip exactly, in reading order, and stay full width.
        let mut cursor = 0;
        for band in bands {
            assert_eq!(band.y_offset, cursor);
            assert_eq!(band.page.width, 360);
            assert_eq!(
                image::load_from_memory(&band.page.bytes).unwrap().height(),
                band.page.height
            );
            cursor += band.page.height;
        }
        assert_eq!(cursor, 6000);

        fs::remove_dir_all(page.parent().unwrap()).expect("remove fixture directory");
    }

    /// 一条切片切出来的全部可观测结果：偏移、高度、字节。
    fn bands_of(imported: &[Imported], stem: &str) -> Vec<(u32, u32, Vec<u8>)> {
        let Imported::Strip { bands, .. } = split(imported, stem) else {
            panic!("{stem} must arrive cut");
        };
        bands
            .iter()
            .map(|band| (band.y_offset, band.page.height, band.page.bytes.to_vec()))
            .collect()
    }

    #[test]
    fn a_setting_of_zero_cuts_exactly_what_no_setting_cuts() {
        let strip = fixture("chapter.png", &strip_image(360, 6000));
        let unset =
            import_webtoon(vec![strip.clone()], Slicing::Auto, AdBands::default()).expect("import");
        let zeroed = import_webtoon(vec![strip.clone()], Slicing::Auto, AdBands { head: 0, tail: 0 })
            .expect("import");

        assert_eq!(
            bands_of(&zeroed.imported, "chapter"),
            bands_of(&unset.imported, "chapter"),
            "a setting of zero must not move a single byte"
        );
        assert_eq!(zeroed.ad_bands.trimmed(), 0);
        assert!(zeroed.ad_bands.skipped().is_empty());

        fs::remove_dir_all(strip.parent().unwrap()).expect("remove fixture directory");
    }

    /// 切片与源图对应区域的平均逐通道绝对差。
    ///
    /// 切片是重编码出来的派生数据，所以它不再与源图逐位相同——这正是它廉价的原因。但仍然必须
    /// 证明它是源图**那一段**的重述而不是别的内容，所以比的是整幅的平均像素差。这里刻意放得很
    /// 松：量错区域会让白纸对上黑块，均值差上百；编码器的边缘振铃只值个位数。
    fn mean_difference(band: &DynamicImage, source_region: &DynamicImage) -> f64 {
        let band = band.to_rgba8();
        let source_region = source_region.to_rgba8();
        assert_eq!(band.dimensions(), source_region.dimensions());
        let pixels = band.width() as f64 * band.height() as f64 * 4.0;
        band.pixels()
            .zip(source_region.pixels())
            .map(|(left, right)| {
                (0..4)
                    .map(|channel| u64::from(left.0[channel].abs_diff(right.0[channel])))
                    .sum::<u64>()
            })
            .sum::<u64>() as f64
            / pixels
    }

    #[test]
    fn the_ad_bands_are_cut_off_before_the_strip_is_sliced() {
        let (head, tail) = (400u32, 300u32);
        let strip = fixture("chapter.png", &strip_image(360, 6000));
        let source = image::open(&strip).expect("read fixture");
        let imported = import_webtoon(vec![strip.clone()], Slicing::Auto, AdBands { head, tail })
            .expect("import");

        assert_eq!(imported.ad_bands.trimmed(), 1);
        assert!(imported.ad_bands.skipped().is_empty());
        let Imported::Strip {
            width,
            height,
            bands,
            ..
        } = split(&imported.imported, "chapter")
        else {
            panic!("a 1:16 image must be cut on import");
        };
        // 条带仍然是未切割的图，因为那才是切片指向的东西。
        assert_eq!((*width, *height), (360, 6000));
        assert!(bands.len() > 1);

        // 切片按阅读顺序铺满中段：第一条从首条广告带之下开始，最后一条停在尾条广告带之上，每一条
        // 都是源图自己的像素。
        let mut cursor = head;
        for band in bands {
            assert_eq!(band.y_offset, cursor, "bands must tile the middle with no gap");
            assert_eq!(band.page.width, 360);
            let cut = image::load_from_memory(&band.page.bytes).expect("decode band");
            assert_eq!(cut.height(), band.page.height);
            let expected = source.crop_imm(0, band.y_offset, 360, band.page.height);
            let difference = mean_difference(&cut, &expected);
            assert!(
                difference < 12.0,
                "band at {} is not a rendering of its own source region ({difference:.2})",
                band.y_offset
            );
            cursor += band.page.height;
        }
        assert_eq!(cursor, 6000 - tail, "the bands must stop at the tail band");

        fs::remove_dir_all(strip.parent().unwrap()).expect("remove fixture directory");
    }

    #[test]
    fn only_the_chapter_s_own_ends_carry_the_ad_bands() {
        // 三张长图是一章：广告只长在首图的顶部和末图的底部。
        let (head, tail) = (400u32, 300u32);
        let first = fixture("page1.png", &strip_image(360, 6000));
        let middle = fixture("page2.png", &strip_image(360, 6000));
        let last = fixture("page3.png", &strip_image(360, 6000));
        let imported = import_webtoon(
            vec![first.clone(), middle.clone(), last.clone()],
            Slicing::Auto,
            AdBands { head, tail },
        )
        .expect("import");

        // 首图只削顶部，末图只削底部，中间那张两侧都是正文。
        let first_bands = bands_of(&imported.imported, "page1");
        let middle_bands = bands_of(&imported.imported, "page2");
        let last_bands = bands_of(&imported.imported, "page3");
        assert_eq!(first_bands.first().unwrap().0, head);
        assert_eq!(first_bands.iter().map(|band| band.1).sum::<u32>(), 6000 - head);
        assert_eq!(middle_bands.first().unwrap().0, 0);
        assert_eq!(middle_bands.iter().map(|band| band.1).sum::<u32>(), 6000);
        assert_eq!(last_bands.first().unwrap().0, 0);
        assert_eq!(last_bands.iter().map(|band| band.1).sum::<u32>(), 6000 - tail);
        // 只有背着自己那一端广告带的两张算被裁过。
        assert_eq!(imported.ad_bands.trimmed(), 2);
        assert!(imported.ad_bands.skipped().is_empty());

        fs::remove_dir_all(first.parent().unwrap()).expect("remove fixture directory");
    }

    #[test]
    fn a_middle_shorter_than_one_page_is_imported_whole_and_reported() {
        // 6000 - 2500 - 2500 剩下 1000 行，小于规划器允许的 1200 最短页。
        let strip = fixture("chapter.png", &strip_image(360, 6000));
        let imported = import_webtoon(
            vec![strip.clone()],
            Slicing::Auto,
            AdBands {
                head: 2500,
                tail: 2500,
            },
        )
        .expect("import");

        assert!(matches!(
            split(&imported.imported, "chapter"),
            Imported::Page(page) if page.height == 6000
        ));
        assert_eq!(imported.ad_bands.trimmed(), 0);
        assert_eq!(imported.ad_bands.skipped().len(), 1);
        assert_eq!(imported.ad_bands.skipped()[0].name, "chapter.png");
        assert_eq!(imported.ad_bands.skipped()[0].reason, AdBandSkip::TooShort);

        fs::remove_dir_all(strip.parent().unwrap()).expect("remove fixture directory");
    }

    #[test]
    fn bands_that_cover_the_image_are_a_setting_mistake() {
        let strip = fixture("chapter.png", &strip_image(360, 6000));
        let covered = import_webtoon(
            vec![strip.clone()],
            Slicing::Auto,
            AdBands {
                head: 3000,
                tail: 3000,
            },
        )
        .expect("import");

        assert!(
            covered.imported.is_empty(),
            "nothing is left of the image to import"
        );
        assert_eq!(covered.ad_bands.trimmed(), 0);
        assert_eq!(covered.ad_bands.skipped().len(), 1);
        assert_eq!(covered.ad_bands.skipped()[0].reason, AdBandSkip::Covered);

        // 广告带是用户输入，所以减法必须失败，而不是回绕成一个看起来合理的中段。
        let overflow = import_webtoon(
            vec![strip.clone()],
            Slicing::Auto,
            AdBands {
                head: u32::MAX,
                tail: u32::MAX,
            },
        )
        .expect("import");
        assert!(overflow.imported.is_empty());
        assert_eq!(overflow.ad_bands.skipped()[0].reason, AdBandSkip::Covered);

        fs::remove_dir_all(strip.parent().unwrap()).expect("remove fixture directory");
    }

    #[test]
    fn an_ordinary_page_is_never_trimmed() {
        let page = fixture("page.png", &strip_image(800, 1600));
        let imported = import_webtoon(
            vec![page.clone()],
            Slicing::Auto,
            AdBands {
                head: 200,
                tail: 200,
            },
        )
        .expect("import");

        assert!(matches!(
            split(&imported.imported, "page"),
            Imported::Page(page) if page.height == 1600
        ));
        assert_eq!(imported.ad_bands.trimmed(), 0);
        // 广告带对一张本来就不切的页连问都没问过，所以没有关于它的报告。
        assert!(imported.ad_bands.skipped().is_empty());

        fs::remove_dir_all(page.parent().unwrap()).expect("remove fixture directory");
    }

    /// 导入切页的分阶段基准。
    ///
    /// 夹具不入库：`KOHARU_IMPORT_BENCH` 给一个目录或一个文件，测试只读不写。默认 `#[ignore]`，
    /// 因为它依赖外部数据且单次要跑很久——与 `xianscan-rust/tests/perf_tall_page.rs` 同一做法。
    ///
    /// 分解那段按 `cut_one` 的调用顺序逐段累加：解码 → 灰度化 → 行剖面 → 规划 → 裁剪 → 重编码。
    /// 整条真实路径随后由 `cut_one` 自己再跑一遍，两者对得上才说明这里量的确实是产品在做的事。
    #[test]
    #[ignore = "needs external fixtures; point KOHARU_IMPORT_BENCH at a strip"]
    fn bench_import_stages() {
        fn ms(duration: Duration) -> f64 {
            duration.as_secs_f64() * 1e3
        }

        let fixture = std::env::var("KOHARU_IMPORT_BENCH")
            .expect("KOHARU_IMPORT_BENCH must name a directory or an image file");
        let fixture = PathBuf::from(fixture);
        let mut paths = if fixture.is_dir() {
            collect_importable(&fixture).expect("collect the benchmark fixtures")
        } else {
            vec![fixture]
        };
        assert!(!paths.is_empty(), "no importable benchmark fixture found");
        alphanumeric_sort::sort_slice_by_os_str_key(&mut paths, |path| {
            path.file_name().unwrap_or_else(|| path.as_os_str())
        });
        let params = SliceParams::default();

        for path in &paths {
            let started = Instant::now();
            let pages = read(vec![path.clone()]).expect("read the benchmark fixture");
            let reading = started.elapsed();
            println!("\n=== {} ===", path.display());
            println!("    read (bytes + header)   {:>9.1} ms", ms(reading));

            for page in pages {
                println!(
                    "\n{}  {}x{}  {:?}  {:.1} MiB source",
                    page.name,
                    page.width,
                    page.height,
                    page.format,
                    page.bytes.len() as f64 / (1024.0 * 1024.0)
                );

                let started = Instant::now();
                let image = image::load_from_memory_with_format(&page.bytes, page.format)
                    .expect("decode the benchmark fixture");
                let decode = started.elapsed();

                let started = Instant::now();
                let luma = image.to_luma8();
                let to_luma = started.elapsed();

                let started = Instant::now();
                let profile = row_profile(&luma);
                let profiling = started.elapsed();

                let started = Instant::now();
                let plan = plan_slices(page.width, page.height, &profile, &params);
                let planning = started.elapsed();

                let mut banding = Duration::ZERO;
                let mut encoded_bytes = 0usize;
                let mut bands = 0usize;
                if let Some(plan) = &plan {
                    // 与 `band` 同样并行，否则这里量的是一个生产路径并不存在的串行版本。
                    let started = Instant::now();
                    let sizes = plan
                        .page_ranges()
                        .into_par_iter()
                        .map(|(_, height)| {
                            let cropped = image.crop_imm(0, 0, plan.width, height);
                            encode(&cropped).expect("encode a band").len()
                        })
                        .collect::<Vec<_>>();
                    banding = started.elapsed();
                    encoded_bytes = sizes.iter().sum();
                    bands = sizes.len();
                }

                let measured = decode + to_luma + profiling + planning + banding;
                println!("    decode                 {:>9.1} ms", ms(decode));
                println!("    to_luma8               {:>9.1} ms", ms(to_luma));
                println!("    row_profile            {:>9.1} ms", ms(profiling));
                println!("    plan_slices            {:>9.1} ms", ms(planning));
                println!("    crop+encode      x{bands:<4}{:>9.1} ms", ms(banding));
                println!(
                    "    bands -> {bands} pages, {:.1} MiB encoded",
                    encoded_bytes as f64 / (1024.0 * 1024.0)
                );
                println!("    measured (production)   {:>9.1} ms", ms(measured));

                // 真实路径：同样的输入走完整的 `cut_one`，用来核对上面这段分解没有跑偏。
                let started = Instant::now();
                let outcome = cut_one(page, &params, AdBands::default());
                let total = started.elapsed();
                let pages_out = match &outcome {
                    CutOutcome::Imported(Imported::Strip { bands, .. })
                    | CutOutcome::ImportedWhole {
                        entry: Imported::Strip { bands, .. },
                        ..
                    } => bands.len(),
                    CutOutcome::Imported(Imported::Page(_))
                    | CutOutcome::ImportedWhole {
                        entry: Imported::Page(_),
                        ..
                    } => 1,
                    CutOutcome::Dropped { .. } => 0,
                };
                println!(
                    "    cut_one (real path)    {:>9.1} ms  -> {pages_out} pages",
                    ms(total)
                );
            }
        }
    }

    /// 真实数据上的整章导入基准：逐章跑完整预处理，并校验切片能否被下游消费。
    ///
    /// 章与章之间**串行**，与 `build_chapter_projects` 一致；章内由 `import_webtoon` 自己并行。
    /// 所以这个数字回答的正是"一口气导入一部漫画"在预处理上要花多久，而不是把十章摊到十份 CPU
    /// 上。夹具目录下的每个子目录算一章。
    #[test]
    #[ignore = "needs external fixtures; point KOHARU_IMPORT_BENCH at a chapter root"]
    fn bench_import_chapters() {
        fn ms(duration: Duration) -> f64 {
            duration.as_secs_f64() * 1e3
        }

        let root = PathBuf::from(std::env::var("KOHARU_IMPORT_BENCH").expect(
            "KOHARU_IMPORT_BENCH must name a directory of chapter folders",
        ));
        let mut chapters = std::fs::read_dir(&root)
            .expect("read the chapter root")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        alphanumeric_sort::sort_slice_by_os_str_key(&mut chapters, |path| {
            path.file_name().unwrap_or_else(|| path.as_os_str())
        });
        assert!(
            !chapters.is_empty(),
            "no chapter folder under {}",
            root.display()
        );
        println!("chapters: {}", chapters.len());

        let params = SliceParams::default();
        // 切片的可用性检查分两层：几何不变量是整数运算，全部跑掉不花钱；"每一片真的能解码出
        // 内容"要重新解码一张图，是导入本身的好几倍重，所以默认只抽查每四片。想跑满抽查时设
        // KOHARU_IMPORT_BENCH_DEEP=1 —— 但那时这个基准量到的就主要是校验而不是导入了。
        let deep = std::env::var("KOHARU_IMPORT_BENCH_DEEP").is_ok();
        let mut strips = 0usize;
        let mut slices = 0usize;
        let mut unsliced = 0usize;
        let mut checked = 0usize;
        let mut source_bytes = 0usize;
        let mut slice_bytes = 0usize;
        let mut slowest = Duration::ZERO;
        let mut import_total = Duration::ZERO;
        let overall = Instant::now();

        for (number, directory) in chapters.iter().enumerate() {
            let files = collect_importable(directory).expect("collect a chapter");
            let files_read = files.len();
            let started = Instant::now();
            let imported = import_webtoon(files, Slicing::Auto, AdBands::default())
                .expect("import a chapter");
            let elapsed = started.elapsed();
            slowest = slowest.max(elapsed);
            import_total += elapsed;
            let checked_started = Instant::now();

            let mut chapter_slices = 0usize;
            for entry in &imported.imported {
                let Imported::Strip {
                    source,
                    width,
                    height,
                    bands,
                    ..
                } = entry
                else {
                    unsliced += 1;
                    continue;
                };
                source_bytes += source.len();
                strips += 1;
                let mut cursor = 0u32;
                for (index, band) in bands.iter().enumerate() {
                    let at = band.y_offset;
                    assert_eq!(
                        at, cursor,
                        "{}: slices must tile the source with no gap",
                        directory.display()
                    );
                    assert!(
                        band.page.height <= params.max_height,
                        "{}: slice at {at} is {} tall, above max_height {}",
                        directory.display(),
                        band.page.height,
                        params.max_height
                    );
                    assert!(
                        band.page.height >= params.min_height || at + band.page.height == *height,
                        "{}: slice at {at} is {} tall, below min_height {}",
                        directory.display(),
                        band.page.height,
                        params.min_height
                    );
                    assert_eq!(band.page.width, *width, "a slice must keep the source width");
                    chapter_slices += 1;
                    slice_bytes += band.page.bytes.len();
                    cursor += band.page.height;
                    if deep && (index % 4 == 0 || index + 1 == bands.len()) {
                        let decoded = image::load_from_memory_with_format(
                            &band.page.bytes,
                            band.page.format,
                        )
                        .unwrap_or_else(|error| {
                            panic!(
                                "{}: slice at {at} does not decode: {error}",
                                directory.display()
                            )
                        });
                        assert_eq!(decoded.height(), band.page.height);
                        let mean = row_profile(&decoded.to_luma8())
                            .iter()
                            .map(|stat| f64::from(stat.mean))
                            .sum::<f64>()
                            / f64::from(decoded.height());
                        assert!(
                            mean > 8.0,
                            "{}: slice at {at} came out black (mean luma {mean:.1})",
                            directory.display()
                        );
                        checked += 1;
                    }
                }
                assert_eq!(
                    cursor, *height,
                    "{}: slices must cover the source exactly",
                    directory.display()
                );
            }
            slices += chapter_slices;

            println!(
                "{:>3}/{}. {:<14} {:>3} files -> {:>4} slices  import {:>9.1} ms  check {:>9.1} ms",
                number + 1,
                chapters.len(),
                directory.file_name().unwrap_or_default().to_string_lossy(),
                files_read,
                chapter_slices,
                ms(elapsed),
                ms(checked_started.elapsed())
            );
        }

        let overall = overall.elapsed();
        println!("\nunsliced images: {unsliced}, slices decoded for checking: {checked}");
        println!(
            "IMPORT total {} chapters, {strips} strips -> {slices} slices in {:.1} s (slowest chapter {:.1} s)",
            chapters.len(),
            ms(overall) / 1e3,
            ms(slowest) / 1e3
        );
        println!(
            "import {:.1} s, checking {:.1} s",
            ms(import_total) / 1e3,
            ms(overall.saturating_sub(import_total)) / 1e3
        );
        println!(
            "source {:.1} MiB -> slices {:.1} MiB",
            source_bytes as f64 / (1024.0 * 1024.0),
            slice_bytes as f64 / (1024.0 * 1024.0)
        );
    }
}
