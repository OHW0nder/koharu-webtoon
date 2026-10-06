//! 条漫（纵向长漫画）支持。
//!
//! 一张纵向长图直接送进检测器，会被压到模型固定的正方形输入上：实测 720×14317 的长条纵向
//! 压缩到 0.08，原本 40px 高的对白框进模型只剩 3px，低于骨干网络的最小可分辨尺度，气泡检出
//! 归零。因此条漫必须在进入流水线之前按可读高度切成若干页；切完之后每个切片就是一张普通页，
//! 检测、OCR、翻译、排版都不需要知道条漫曾经存在。
//!
//! 本模块只用内核已公开的扩展点实现，不改动 `koharu-scene`：
//!
//! - 切片溯源存在自定义组件里。内核对未注册的组件 kind 兜底放行，并保留其 schema 版本、
//!   引用与指纹，所以官方版本加载本项目不会失败，切片数据也不会丢失。
//! - 切割规划在 `koharu_ml::webtoon`，是纯函数，与持久化无关。
//!
//! kind 是 `dev.koharu.webtoon.page.slice`，与项目其余组件同属 `dev.koharu` 命名空间。命名空间会写进项目数据，一旦
//! 有数据落盘就不能更改，改名会让既有项目读不到自己的切片信息。
//!
//! 形态选择说明：Koharu 没有插件或可替换流程的扩展点，导入路径是硬编码的单条命令。因此这里
//! 的扩展形式是"新增一条并列的导入命令"，而不是"注册一个导入器去替换既有命令"——后者会要求
//! 修改既有命令签名，并让前端的协议文件与上游永久分叉。

use std::sync::Arc;

use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, At, BlobId, Component, Edit, EntityId, Error as SceneError,
    PageDraft, RemovePolicy, Result as SceneResult, ValidationContext,
};
use revision::revisioned;
use serde::{Deserialize, Serialize};
use specta::Type;

/// 承载源图的临时页面在删除前不会出现在任何快照里，因此这个标签只用于日志与调试。
const CARRIER_LABEL: &str = "strip source";

/// 标记一个页面是纵向长图的某个切片。
///
/// 这是条漫在本项目里留下的唯一记录，它回答"这一页的像素从哪张图切出来"。
#[revisioned(revision = 1)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct PageSlice {
    /// 未切割源图的内容寻址标识。
    ///
    /// 溯源锚定不可变的字节而不是源页实体，这样用户删掉承载源图的页面之后，切片仍能追溯回
    /// 它来自哪张图，那张图也仍留在项目里，可以按新的边界重新切割。声明为 blob 引用之后，
    /// 即使没有任何页面显示这张源图，垃圾回收也会因为切片仍然指名它而把它保留下来。
    pub source: BlobId,
    /// 源图宽度；切片与它相同。
    pub source_width: f64,
    /// 源图高度。
    pub source_height: f64,
    /// 本切片顶端到源图顶端的距离。
    pub y_offset: f64,
    /// 本切片高度。
    pub slice_height: f64,
}

impl Component for PageSlice {
    const KIND: &'static str = "dev.koharu.webtoon.page.slice";

    fn blob_refs(&self) -> Vec<BlobId> {
        vec![self.source]
    }

    fn validate(&self, context: &ValidationContext<'_>) -> SceneResult<()> {
        let source = self.source_width.is_finite()
            && self.source_width > 0.0
            && self.source_height.is_finite()
            && self.source_height > 0.0;
        // 越界的切片会把 OCR 与排版的几何放到画面之外，所以这是组件的不变量而非调用者的义务。
        let inside_source = self.y_offset.is_finite()
            && self.slice_height.is_finite()
            && self.y_offset >= 0.0
            && self.slice_height > 0.0
            && self.y_offset + self.slice_height <= self.source_height;
        if source && inside_source && context.contains_blob(self.source) {
            Ok(())
        } else {
            Err(SceneError::Invalid(
                "webtoon slice does not lie inside its recorded source image".to_owned(),
            ))
        }
    }
}

/// 一条待导入的长图，以及已经规划好的切片。
pub struct StripInput {
    /// 未切割的原始字节。
    pub bytes: Arc<[u8]>,
    pub width: f64,
    pub height: f64,
    pub media_type: String,
    /// 按阅读顺序排列的切片。
    pub bands: Vec<BandInput>,
}

/// 长图的一个切片。
pub struct BandInput {
    pub label: String,
    /// 顶端到源图顶端的距离。
    pub y_offset: f64,
    pub height: f64,
    /// 裁切并重新编码后的图片字节。
    pub bytes: Arc<[u8]>,
    pub media_type: String,
}

/// 把一条长图的切片作为普通页插入项目。
///
/// 切片在同一批编辑里创建：半条长图不是一个有意义的文档，而且它们的页面位置是一个整体
/// 意图，必须作为一个单位参与并发冲突检测，而不是拆成 N 次互不相干的插入。
///
/// 源图不作为页面留下，但它的字节必须进入项目存储，否则切片无从溯源、也无法重新切割。做法
/// 是借一个临时页面把字节挂进存储再删掉它：内容寻址的 blob 身份可以预先算出，切片的 blob
/// 引用会把它钉住，所以删页不会让垃圾回收把它带走。这个临时页面必须先于切片建立，否则切片
/// 组件的校验会认为源图还不存在。
///
/// 页面位置按插入顺序推进，整批落在请求位置之后的连续区间里，与内核一次性插入 N 页的
/// 结果一致。
pub fn add_strip(edit: &mut Edit, strip: StripInput, at: At) -> SceneResult<Vec<EntityId>> {
    if strip.bands.is_empty() {
        return Err(SceneError::Invalid(
            "a sliced source image must produce at least one page".to_owned(),
        ));
    }
    let source = BlobId::for_bytes(&strip.bytes);

    let carrier = edit.add_page(
        PageDraft::new(CARRIER_LABEL, strip.width, strip.height),
        At::End,
    )?;
    edit.set_asset(
        carrier,
        &AssetRole::new("source")?,
        AssetInput::new(
            strip.bytes,
            strip.media_type,
            AssetMetadata {
                width: Some(strip.width.round() as u32),
                height: Some(strip.height.round() as u32),
                attributes: Default::default(),
            },
        ),
    )?;

    let mut pages = Vec::with_capacity(strip.bands.len());
    for band in strip.bands {
        let placement = match pages.last() {
            Some(previous) => At::After(*previous),
            None => at,
        };
        let page = edit.add_page(
            PageDraft::new(band.label, strip.width, band.height),
            placement,
        )?;
        // 切片就是这一页显示的图，所以资产尺寸按切片标注，而不是相信调用方：页面与它的画面
        // 不一致会让后面每一级的检测都落在错误的位置上。
        edit.set_asset(
            page,
            &AssetRole::new("source")?,
            AssetInput::new(
                band.bytes,
                band.media_type,
                AssetMetadata {
                    width: Some(strip.width.round() as u32),
                    height: Some(band.height.round() as u32),
                    attributes: Default::default(),
                },
            ),
        )?;
        edit.set(
            page,
            &PageSlice {
                source,
                source_width: strip.width,
                source_height: strip.height,
                y_offset: band.y_offset,
                slice_height: band.height,
            },
        )?;
        pages.push(page);
    }

    edit.remove_entity(carrier, RemovePolicy::Cascade)?;
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use koharu_scene::{Error as SceneError, Session};

    use super::*;

    const UNCUT: &[u8] = b"uncut-strip";

    fn strip(bands: Vec<BandInput>) -> StripInput {
        StripInput {
            bytes: Arc::from(UNCUT),
            width: 720.0,
            height: 3000.0,
            media_type: "image/png".to_owned(),
            bands,
        }
    }

    fn band(index: usize, y_offset: f64, height: f64) -> BandInput {
        BandInput {
            label: format!("panel {index}"),
            y_offset,
            height,
            bytes: Arc::from(vec![index as u8; 16]),
            media_type: "image/png".to_owned(),
        }
    }

    async fn import(bands: Vec<BandInput>) -> koharu_scene::Snapshot {
        let mut session = Session::memory().await.unwrap();
        let patch = session
            .snapshot()
            .patch(|edit| {
                add_strip(edit, strip(bands), At::End)?;
                Ok(())
            })
            .unwrap();
        session.commit(patch).await.unwrap().snapshot
    }

    #[tokio::test]
    async fn a_strip_becomes_ordered_pages_carrying_their_band() {
        let snapshot = import(vec![
            band(1, 0.0, 1000.0),
            band(2, 1000.0, 1000.0),
            band(3, 2000.0, 1000.0),
        ])
        .await;

        let pages = snapshot
            .pages()
            .map(|page| {
                let value = page.page().unwrap();
                let slice = snapshot.component::<PageSlice>(page.id()).unwrap().unwrap();
                (value, slice)
            })
            .collect::<Vec<_>>();

        // The carrier page is an implementation detail and must not reach the page rail.
        assert_eq!(pages.len(), 3);
        for (index, (value, slice)) in pages.iter().enumerate() {
            assert_eq!(value.label, format!("panel {}", index + 1));
            assert_eq!(value.width, 720.0);
            assert_eq!(value.height, 1000.0);
            assert_eq!(slice.source, BlobId::for_bytes(UNCUT));
            assert_eq!(slice.source_width, 720.0);
            assert_eq!(slice.source_height, 3000.0);
            assert_eq!(slice.y_offset, index as f64 * 1000.0);
            assert_eq!(slice.slice_height, 1000.0);
        }
    }

    #[tokio::test]
    async fn the_uncut_image_survives_the_carrier_page_being_removed() {
        // Nothing displays the uncut image once the carrier page is gone, so the bands' blob
        // reference is the only thing keeping it in the project. Without it an imported chapter
        // could never be re-cut.
        let source = BlobId::for_bytes(UNCUT);
        let snapshot = import(vec![band(1, 0.0, 3000.0)]).await;

        assert_eq!(snapshot.pages().count(), 1);
        assert!(snapshot.read_blob(source).await.is_ok());
    }

    #[tokio::test]
    async fn a_strip_without_bands_is_rejected() {
        let session = Session::memory().await.unwrap();
        let error = session
            .snapshot()
            .patch(|edit| {
                add_strip(edit, strip(Vec::new()), At::End)?;
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(error, SceneError::Invalid(_)), "{error:?}");
    }

    #[tokio::test]
    async fn a_band_reaching_past_the_source_is_rejected() {
        let session = Session::memory().await.unwrap();
        let error = session
            .snapshot()
            .patch(|edit| {
                add_strip(edit, strip(vec![band(1, 2000.0, 1500.0)]), At::End)?;
                Ok(())
            })
            .unwrap_err();
        assert!(matches!(error, SceneError::Invalid(_)), "{error:?}");
    }
}