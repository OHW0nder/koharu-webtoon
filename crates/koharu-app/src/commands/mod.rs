pub(crate) mod agent;
pub(crate) mod canvas;
pub(crate) mod editing;
pub(crate) mod fonts;
pub(crate) mod import;
pub(crate) mod lifecycle;
pub(crate) mod output;
pub(crate) mod preferences;
pub(crate) mod processing;
pub(crate) mod project;
pub(crate) mod series;
pub(crate) mod source;

use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use tauri::ipc::{Channel, IpcResponse};

#[derive(Debug, Type)]
#[specta(transparent)]
pub(crate) struct Error(#[specta(type = String)] anyhow::Error);

impl<E> From<E> for Error
where
    E: Into<anyhow::Error>,
{
    fn from(error: E) -> Self {
        Self(error.into())
    }
}

impl Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&format!("{:#}", self.0))
    }
}

use processing::Processing;

/// 把带 alpha 的页合成到白纸上，得到 JPEG 能吃的 RGB。
///
/// JPEG 没有 alpha 通道，而丢掉它会让透明区域变成像素里存的那几个 RGB 值——通常是黑的，
/// 于是白纸变黑纸。切片编码与 CBZ 导出共用这一份，所以两条出 JPEG 的路对透明的处理不会分叉。
pub(crate) fn flatten_onto_white(rgba: &image::RgbaImage) -> image::RgbImage {
    let mut rgb = image::RgbImage::new(rgba.width(), rgba.height());
    for (target, source) in rgb.pixels_mut().zip(rgba.pixels()) {
        let alpha = f32::from(source.0[3]) / 255.0;
        if alpha < 1.0 {
            for channel in 0..3 {
                target.0[channel] =
                    (f32::from(source.0[channel]) * alpha + 255.0 * (1.0 - alpha)).round() as u8;
            }
        } else {
            target.0.copy_from_slice(&source.0[..3]);
        }
    }
    rgb
}

pub(crate) trait ChannelExt<T> {
    fn publish(&self, value: T);
}

/// Rejects a page import while a processing job is running.
///
/// Both contend for the same project commit sequence, so this is the one guard every import path
/// goes through regardless of whether it opens a project, a shelf, or a whole series.
pub(crate) fn reject_import_while_processing(
    processing: &Processing,
) -> std::result::Result<(), Error> {
    if processing.is_running() {
        Err(anyhow::anyhow!("pages cannot be imported while processing is running").into())
    } else {
        Ok(())
    }
}

/// Rejects a settings write while a processing job is running.
///
/// **不是「改了不生效」而是「会把正在跑的东西改坏」。** 跑着的作业读的是管线那份内存配置，而
/// [`preferences::save_preferences`] 是整体替换：它会把批量循环刚装进去的附加说明与上文窗口一起覆盖掉，
/// 于是本章之后每一章都静默地按用户设置跑，批完之后界面看着像生效了，实际没有。漫画级资料在批量开始
/// 时已快照，中途改不会影响这一批，但会让界面与正在跑的内容对不上，所以一并挡住。
pub(crate) fn reject_settings_while_processing(
    processing: &Processing,
) -> std::result::Result<(), Error> {
    if processing.is_running() {
        Err(anyhow::anyhow!("settings cannot be changed while processing is running").into())
    } else {
        Ok(())
    }
}

impl<T: IpcResponse> ChannelExt<T> for Mutex<Option<Channel<T>>> {
    fn publish(&self, value: T) {
        let mut channel = self.lock();
        if channel
            .as_ref()
            .is_some_and(|channel| channel.send(value).is_err())
        {
            channel.take();
        }
    }
}

pub fn bindings() -> tauri_specta::Builder<tauri_runtime_cef::CefRuntime> {
    use tauri_specta::{Builder, ErrorHandlingMode, collect_commands};

    Builder::new()
        .commands(collect_commands![
            agent::get_agent_status,
            agent::login_agent,
            agent::logout_agent,
            agent::save_agent_config,
            agent::run_agent,
            agent::cancel_agent,
            lifecycle::subscribe,
            lifecycle::get_project,
            lifecycle::get_pages,
            lifecycle::get_page,
            lifecycle::open_chapter,
            lifecycle::close_project,
            series::list_series,
            series::get_series,
            series::get_series_settings,
            series::set_series_settings,
            series::get_glossary,
            series::set_glossary,
            series::import_series,
            series::import_series_chapter,
            series::pick_chapter_folder,
            series::delete_series_chapter,
            series::delete_series,
            series::list_orphaned_chapters,
            series::delete_orphaned_chapter,
            series::rename_series,
            series::process_series_chapters,
            series::export_series_chapters,
            source::set_series_source,
            source::check_series_updates,
            source::start_fetch,
            source::cancel_fetch,
            lifecycle::select_page,
            editing::rename_page,
            editing::delete_pages,
            editing::move_page,
            editing::set_source_text,
            editing::set_translation,
            editing::set_typography,
            editing::set_geometry,
            editing::set_visibility,
            editing::delete_layers,
            editing::move_layer,
            editing::undo,
            editing::redo,
            processing::process,
            processing::stop_job,
            output::export,
            output::get_thumbnail,
            fonts::get_fonts,
            fonts::get_font_preview,
            preferences::save_preferences,
            preferences::get_preferences,
            preferences::get_translation_models,
            preferences::get_library_root,
            preferences::set_library_root,
            preferences::pick_library_folder,
            canvas::get_canvas_manifest,
            canvas::get_canvas_resource,
            canvas::prepare_canvas_page,
            canvas::get_canvas_page_manifest,
            canvas::get_canvas_page_resource,
            canvas::add_point_text,
            canvas::add_text_box,
            canvas::commit_paint,
            canvas::commit_erase,
            canvas::commit_transform,
            canvas::commit_inpaint,
        ])
        .disable_serde_phases()
        .error_handling(ErrorHandlingMode::Throw)
}
