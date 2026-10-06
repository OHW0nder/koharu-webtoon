use anyhow::{Context as _, Result};
use futures::future::try_join_all;
use image::{
    ExtendedColorType, ImageEncoder as _,
    codecs::png::{CompressionType, FilterType, PngEncoder},
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

use super::{Error, project::CurrentProject};
use koharu_desktop::Desktop;

const THUMBNAIL_EDGE: u32 = 128;

#[derive(Type)]
#[specta(transparent)]
pub(crate) struct ThumbnailBytes(#[specta(type = Vec<u8>)] Vec<u8>);

impl IpcResponse for ThumbnailBytes {
    fn body(self) -> tauri::Result<tauri::ipc::InvokeResponseBody> {
        Ok(self.0.into())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Png,
    Psd,
    Cbz,
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
    let (name, snapshot) = {
        let project = project.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        (project.name.clone(), project.snapshot())
    };
    if snapshot.pages().next().is_none() {
        return Err(anyhow::anyhow!("there are no pages to export").into());
    }
    let Some(destination) = pick_destination(&window, format, &name).await else {
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
    let (extension, images) = match format {
        ExportFormat::Png | ExportFormat::Cbz => {
            let images = tokio_rayon::spawn(move || {
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
            })
            .await?;
            ("png", images)
        }
        ExportFormat::Psd => {
            let options = PsdExportOptions::default();
            let images = try_join_all(
                frames
                    .iter()
                    .map(|frame| export_page(Arc::clone(&rasterizer), &snapshot, frame, &options)),
            )
            .await?;
            ("psd", images)
        }
    };
    let mut names = Vec::with_capacity(pages.len());
    for page_id in pages {
        names.push(snapshot.page(page_id)?.page()?.label.clone());
    }
    Ok(extension_names(names, extension)
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
        let mut archive = if matches!(format, ExportFormat::Cbz) {
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
                // PNG data is already compressed.
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
