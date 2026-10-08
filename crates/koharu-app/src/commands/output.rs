use anyhow::{Context as _, Result};
use futures::future::try_join_all;
use image::{
    ExtendedColorType, ImageEncoder as _,
    codecs::{
        jpeg::JpegEncoder,
        png::{CompressionType, FilterType, PngEncoder},
    },
};
use koharu_psd::{PsdExportOptions, export_page};
use koharu_rasterizer::{Raster, RasterOptions, Rasterizer};
use koharu_renderer::{Frame, Renderer};
use koharu_scene::{AssetRole, EntityId, Snapshot};
use rayon::prelude::*;
use serde::Deserialize;
use specta::Type;
use std::{io::Write as _, sync::Arc};
use tauri::{State, WebviewWindow, ipc::IpcResponse};
use tauri_runtime_cef::CefRuntime;

use super::{Error, flatten_onto_white, project::CurrentProject};
use koharu_desktop::Desktop;

const THUMBNAIL_EDGE: u32 = 128;

/// 导出 CBZ 时的 JPEG 质量。
///
/// 90 是实测点，不是惯例：在项目自己的真实切片上，同一批像素编成 JPEG q90 是 0.19 B/px，
/// 编成无损 RGBA8 PNG 是 0.80 B/px——四分之一的体积，而漫画内容上看不出差别。再往上体积
/// 回涨得很快，再往下细描边和网点开始糊。
const EXPORT_JPEG_QUALITY: u8 = 90;

#[derive(Type)]
#[specta(transparent)]
pub(crate) struct ThumbnailBytes(#[specta(type = Vec<u8>)] Vec<u8>);

impl IpcResponse for ThumbnailBytes {
    fn body(self) -> tauri::Result<tauri::ipc::InvokeResponseBody> {
        Ok(self.0.into())
    }
}

/// 一个页面的字节表示。
///
/// 它和容器是分开的两件事，因为两者的最佳组合并不是同一种格式。
enum PageCodec {
    Png,
    Jpeg,
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    /// 一个目录的无损 PNG。
    Png,
    /// 一个目录的分层 PSD。
    Psd,
    /// 单文件 CBZ，内含 JPEG。
    Cbz,
    /// 单文件 ZIP，内含无损 PNG。
    Zip,
}

impl ExportFormat {
    /// 页落进归档，还是散在目录里。
    ///
    /// 归档一律用 Stored：两种页编码都已经压过一道，再 deflate 一遍买不到字节。
    fn archived(self) -> bool {
        matches!(self, Self::Cbz | Self::Zip)
    }

    /// 页文件名的扩展名，必须跟着容器里真正的编码走。CBZ 里写 `.png` 不是命名不讲究，
    /// 而是会让按扩展名选解码分支的阅读器挑错。
    fn extension(self) -> &'static str {
        match self {
            Self::Cbz => "jpg",
            Self::Png | Self::Zip => "png",
            Self::Psd => "psd",
        }
    }

    fn codec(self) -> PageCodec {
        match self {
            Self::Cbz => PageCodec::Jpeg,
            Self::Png | Self::Psd | Self::Zip => PageCodec::Png,
        }
    }
}

#[tracing::instrument(
    target = "koharu_metrics",
    name = "export",
    skip_all,
    fields(origin = "user", format = ?format),
)]
#[tauri::command]
#[specta::specta]
pub(crate) async fn export(
    window: WebviewWindow<CefRuntime>,
    format: ExportFormat,
    project: State<'_, CurrentProject>,
    desktop: State<'_, Desktop>,
) -> std::result::Result<(), Error> {
    let (label, snapshot) = {
        let project = project.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        (project.label.clone(), project.snapshot())
    };
    if snapshot.pages().next().is_none() {
        return Err(anyhow::anyhow!("there are no pages to export").into());
    }
    let Some(destination) = pick_destination(&window, format, &label).await else {
        return Ok(());
    };
    export_snapshot(snapshot, format, destination, &desktop).await?;
    Ok(())
}

/// Asks where an export should land. `None` means the user cancelled.
async fn pick_destination(
    window: &WebviewWindow<CefRuntime>,
    format: ExportFormat,
    name: &str,
) -> Option<std::path::PathBuf> {
    let dialog = rfd::AsyncFileDialog::new().set_parent(window);
    let picked = match format {
        ExportFormat::Png | ExportFormat::Psd => dialog.pick_folder().await,
        ExportFormat::Cbz => {
            dialog
                .add_filter("Comic Book Archive", &["cbz"])
                .set_file_name(format!("{name}.cbz"))
                .save_file()
                .await
        }
        ExportFormat::Zip => {
            dialog
                .add_filter("ZIP Archive", &["zip"])
                .set_file_name(format!("{name}.zip"))
                .save_file()
                .await
        }
    };
    picked.map(|destination| destination.path().to_owned())
}

/// Renders every page of one project and writes the result.
///
/// The pages keep the names they were imported under — a folder of `00101.jpg`, `00102.jpg` comes
/// back out under exactly those names, because a reader who compares the export against the source
/// folder should find the same files, and because a page number this code invents is one more thing
/// that can disagree with the original.
pub(crate) async fn export_snapshot(
    snapshot: Snapshot,
    format: ExportFormat,
    destination: std::path::PathBuf,
    desktop: &Desktop,
) -> Result<()> {
    let pages = render_pages(snapshot, format, desktop).await?;
    write_output(pages, format, destination, &[]).await
}

/// Renders one project into encoded pages, in project order.
pub(crate) async fn render_pages(
    snapshot: Snapshot,
    format: ExportFormat,
    desktop: &Desktop,
) -> Result<Vec<(String, Vec<u8>)>> {
    let pages = snapshot.pages().map(|page| page.id()).collect::<Vec<_>>();
    if pages.is_empty() {
        return Err(anyhow::anyhow!("there are no pages to export"));
    }
    let renderer = desktop.renderer();
    let rasterizer = desktop.rasterizer().await?;
    let frames = try_join_all(pages.iter().map(|&page| renderer.render(&snapshot, page))).await?;
    let mut names = Vec::with_capacity(pages.len());
    for page_id in pages {
        names.push(snapshot.page(page_id)?.page()?.label.clone());
    }

    // PSD 是分层交换格式：它的页字节不来自光栅化后的整页位图，所以它不参与下面那一支。
    // 先走掉，否则 PNG 会白跑一遍全项目光栅化再被丢掉。
    if matches!(format, ExportFormat::Psd) {
        let options = PsdExportOptions::default();
        let images = try_join_all(
            frames
                .iter()
                .map(|frame| export_page(Arc::clone(&rasterizer), &snapshot, frame, &options)),
        )
        .await?;
        return Ok(extension_names(names, format.extension())
            .into_iter()
            .zip(images)
            .collect());
    }

    let images = match format.codec() {
        PageCodec::Png => tokio_rayon::spawn({
            let rasterizer = Arc::clone(&rasterizer);
            move || {
                frames
                    .par_iter()
                    .map(|frame| -> Result<_> {
                        let image = rasterizer
                            .rasterize(&frame.raster_frame()?, RasterOptions::default())?
                            .image;
                        let mut bytes = Vec::new();
                        PngEncoder::new_with_quality(
                            &mut bytes,
                            CompressionType::Best,
                            FilterType::Adaptive,
                        )
                        .write_image(
                            image.as_raw(),
                            image.width(),
                            image.height(),
                            ExtendedColorType::Rgba8,
                        )?;
                        Ok(bytes)
                    })
                    .collect::<Result<Vec<_>>>()
            }
        })
        .await?,
        PageCodec::Jpeg => tokio_rayon::spawn({
            let rasterizer = Arc::clone(&rasterizer);
            move || {
                frames
                    .par_iter()
                    .map(|frame| -> Result<_> {
                        let image = rasterizer
                            .rasterize(&frame.raster_frame()?, RasterOptions::default())?
                            .image;
                        // 光栅化的结果带 alpha，而 JPEG 没有 alpha 通道：先合成到白纸上。
                        // 直接丢通道会把那些像素里存的 RGB 露出来——通常是黑的，于是白纸变黑纸。
                        let rgb = flatten_onto_white(&image);
                        let mut bytes = Vec::new();
                        JpegEncoder::new_with_quality(&mut bytes, EXPORT_JPEG_QUALITY)
                            .write_image(
                                rgb.as_raw(),
                                rgb.width(),
                                rgb.height(),
                                ExtendedColorType::Rgb8,
                            )?;
                        Ok(bytes)
                    })
                    .collect::<Result<Vec<_>>>()
            }
        })
        .await?,
    };

    Ok(extension_names(names, format.extension())
        .into_iter()
        .zip(images)
        .collect())
}

/// A page label as an archive entry name: no directory part, and nothing an archive would reject.
fn extension_names(names: Vec<String>, extension: &str) -> Vec<String> {
    names
        .into_iter()
        .map(|name| {
            let stem = name
                .trim()
                .rsplit_once('.')
                .map_or(name.trim(), |(stem, _)| stem);
            let stem = stem.replace(['<', '>', ':', '"', '/', '\\', '|', '?', '*'], "_");
            let stem = stem.trim_end_matches(['.', ' ']);
            format!("{}.{extension}", if stem.is_empty() { "page" } else { stem })
        })
        .collect()
}

/// Writes rendered pages to a folder or a single archive.
///
/// `folders` places the pages inside a directory in the archive, which is what separates a volume
/// export from a single-chapter one: the chapter's own export *is* the chapter, while a volume keeps
/// each chapter in a folder of its own so unpacking it reproduces the shelf.
async fn write_output(
    pages: Vec<(String, Vec<u8>)>,
    format: ExportFormat,
    destination: std::path::PathBuf,
    folders: &[String],
) -> Result<()> {
    let prefix = folders.iter().fold(String::new(), |mut acc, folder| {
        acc.push_str(folder);
        acc.push('/');
        acc
    });
    let folders = folders.to_vec();
    tokio_rayon::spawn(move || -> Result<()> {
        let mut archive = if format.archived() {
            let directory = destination.parent().context("archive path has no parent")?;
            Some(zip::ZipWriter::new(tempfile::NamedTempFile::new_in(
                directory,
            )?))
        } else {
            for folder in &folders {
                std::fs::create_dir_all(destination.join(folder))?;
            }
            None
        };
        for (name, bytes) in pages {
            let entry = format!("{prefix}{name}");
            if let Some(archive) = &mut archive {
                // 两种页编码都已经压过一道（JPEG 是 DCT，PNG 是 deflate），
                // 再 deflate 一遍买不到字节，只会让 CPU 白转。
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
                archive.start_file(entry, options)?;
                archive.write_all(&bytes)?;
            } else {
                std::fs::write(destination.join(entry), bytes)?;
            }
        }
        if let Some(archive) = archive {
            archive.finish()?.persist(destination)?;
        }
        Ok(())
    })
    .await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_thumbnail(
    page: EntityId,
    project: State<'_, CurrentProject>,
) -> std::result::Result<ThumbnailBytes, Error> {
    let snapshot = project
        .project
        .lock()
        .await
        .as_ref()
        .context("no project is open")?
        .snapshot();
    snapshot.page(page)?;
    let blob = snapshot
        .asset(page, &AssetRole::new("source")?)?
        .with_context(|| format!("page {page} has no source image"))?
        .blob;
    let bytes = snapshot.read_blob(blob).await?;
    let bytes = tokio_rayon::spawn(move || -> Result<Vec<u8>> {
        let image = image::load_from_memory(&bytes).context("failed to decode source image")?;
        if image.width() == 0 || image.height() == 0 {
            return Err(anyhow::anyhow!("source image is empty"));
        }
        let image = image.thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE).to_rgba8();
        let encoder = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
        Ok(encoder.encode(80.0).to_vec())
    })
    .await?;
    Ok(ThumbnailBytes(bytes))
}

pub(crate) async fn rendered_preview(
    renderer: &Renderer,
    rasterizer: Arc<Rasterizer>,
    snapshot: &Snapshot,
    page: EntityId,
) -> Result<Vec<u8>> {
    snapshot.page(page)?;
    let frame = renderer.render(snapshot, page).await?;
    let image = rasterize(rasterizer, &frame, RasterOptions::default())
        .await?
        .image;
    tokio_rayon::spawn(move || {
        let image = image::DynamicImage::ImageRgba8(image)
            .resize(1024, 1024, image::imageops::FilterType::Lanczos3)
            .to_rgba8();
        let encoder = webp::Encoder::from_rgba(image.as_raw(), image.width(), image.height());
        Ok::<_, anyhow::Error>(encoder.encode(85.0).to_vec())
    })
    .await
}

async fn rasterize(
    rasterizer: Arc<Rasterizer>,
    frame: &Frame,
    options: RasterOptions,
) -> Result<Raster> {
    let frame = frame.raster_frame()?;
    tokio_rayon::spawn(move || rasterizer.rasterize(&frame, options))
        .await
        .map_err(Into::into)
}
