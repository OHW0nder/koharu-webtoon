//! 阅读页里图片清单的提取，以及图片地址的扩展名。
//!
//! 图片清单不在 JSON 接口后面，只存在于阅读页 HTML 内嵌的 Next.js RSC payload 里。
//! 这是整套实现里唯一跟着站点 markup 走的地方，所以匹配全部收在这一个文件，样本测试也在这里：
//! 站点一改，先改这里，再重新抓一份样本。
//!
//! 这一层只管抽结构，不判断响应像不像话——体积下限是传输层的事，留在 [`super`] 那侧。

use anyhow::{Context as _, Result};

/// 图片数组的起始标记。
///
/// 必须带上 `{"images":[` 而不只是 `chapter_data`：payload 里 `chapter_data` 出现两次，
/// 第一次是 React 流式的延迟引用（形如 `\"chapter_data\":\"$29\"`），指向后面真正的那个 chunk。
/// 只找 `chapter_data` 会读到 `$29` 就以为读到了。
const IMAGES_MARKER: &str = r#"\"chapter_data\":{\"images\":["#;

/// 一章的图片清单。
#[derive(Clone, Debug)]
pub struct ChapterPages {
    /// 按站点给出的页序。
    pub urls: Vec<String>,
    /// 被截断、无法使用的地址数量。它们混在清单里，跳过即可，但数量要报出来：
    /// 突然变多说明站点在改数据格式，而不是网络在抖。
    pub invalid: usize,
}

/// 从阅读页 HTML 里取出图片清单。
///
/// 空清单是合法结果：站点认得这个字段却不给图，通常是付费或尚未解锁。
/// 找不到字段才是失败——那说明 markup 变了，调用方要能把这两种情况分开。
pub fn chapter_pages(html: &str) -> Result<ChapterPages> {
    let start = html.find(IMAGES_MARKER).context(
        "the chapter page carries no image list, so the site markup likely changed",
    )?;
    let body = html
        .get(start + IMAGES_MARKER.len()..)
        .context("the image list starts outside the document")?;

    // 只还原这一小段里的转义引号，不重写整页：整页有几十 KB 与清单无关的内容，
    // 而清单之外再解一次转义只会引入误伤。
    let listed: Vec<String> = {
        let end = body
            .find(']')
            .context("the image list is never closed, so the site markup likely changed")?;
        let raw = body
            .get(..end)
            .context("the image list ends outside the document")?;
        serde_json::from_str::<Vec<String>>(&format!("[{raw}]").replace("\\\"", "\""))
            .context("the image list is not valid JSON, so the site markup likely changed")?
    };

    let mut urls = Vec::with_capacity(listed.len());
    let mut invalid = 0;
    for entry in listed {
        // 站点数据里出现过只到 `htt` 的截断地址。它们要等到下载时才暴露成失败，
        // 在这里滤掉可以省掉一轮注定失败的重试。
        if entry.starts_with("http://") || entry.starts_with("https://") {
            urls.push(entry);
        } else {
            invalid += 1;
        }
    }
    Ok(ChapterPages { urls, invalid })
}

/// 图片地址的扩展名，归一成 `import` 认识的形式。
///
/// **有意收窄**：上游那份实现还放行 `gif` / `avif`，但 koharu 的 `Format` 只认
/// png / jpg / jpeg / webp，落成别的扩展名会被 `collect_importable` 静默丢掉，
/// 表现是「这一章导入进来是空的」。认不出来的一律当 jpg：解码读的是内容字节，
/// 不是扩展名，所以即使真拿到 gif 也能解出来。
pub fn image_extension(url: &str) -> &'static str {
    let path = match url.split_once(['?', '#']) {
        Some((path, _)) => path,
        None => url,
    };
    match path.rsplit('.').next() {
        Some(extension) if extension.eq_ignore_ascii_case("png") => "png",
        Some(extension) if extension.eq_ignore_ascii_case("webp") => "webp",
        _ => "jpg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10 抓自 `omegascans.org` 的一章阅读页，裁到只剩目标 chunk，
    /// 并把上传路径与文件名换成占位。
    const CAPTURE: &str = include_str!("testdata/chapter-page.rsc");

    #[test]
    fn reads_a_real_capture() {
        let pages = chapter_pages(CAPTURE).expect("the capture should parse");
        assert_eq!(pages.urls.len(), 7);
        assert_eq!(pages.invalid, 0);
        assert!(
            pages.urls[0].ends_with("01-cqkx9tac5xbkl8kii9xwcxx1.jpg"),
            "page order must be preserved, got {:?}",
            pages.urls[0]
        );
        assert!(pages.urls[6].ends_with("07-xmpwmpnlj6j7wdaqhc9c7iyl.jpg"));
    }

    #[test]
    fn skips_the_deferred_reference_that_precedes_the_real_list() {
        // 活页里 `chapter_data` 的第一次出现是 React 流式的延迟引用，指向后面的 chunk。
        let payload = concat!(
            r#"self.__next_f.push([1,\"chapter_data\":\"$29\",\"chapter_slug\":\"chapter-36\""])"#,
            "\n",
            r#"self.__next_f.push([1,\"chapter_data\":{\"images\":[\"https://cdn.test/a.jpg\"]}"#,
        );
        let pages = chapter_pages(payload).expect("the deferred pointer must not shadow the list");
        assert_eq!(pages.urls, ["https://cdn.test/a.jpg"]);
    }

    #[test]
    fn counts_the_addresses_the_site_truncated() {
        let payload =
            r#"\"chapter_data\":{\"images\":[\"https://cdn.test/a.jpg\",\"htt\",\"png\",\"https://cdn.test/b.png\"]}"#;
        let pages = chapter_pages(payload).expect("a truncated list is still a list");
        assert_eq!(pages.urls, ["https://cdn.test/a.jpg", "https://cdn.test/b.png"]);
        assert_eq!(pages.invalid, 2);
    }

    #[test]
    fn tells_an_empty_list_apart_from_a_changed_markup() {
        // 空数组说明站点认得这个字段却没给图，通常是付费或未解锁，不是 markup 变了。
        let empty = chapter_pages(r#"\"chapter_data\":{\"images\":[]}"#).expect("an empty list parses");
        assert!(empty.urls.is_empty());
        assert_eq!(empty.invalid, 0);

        assert!(chapter_pages("<html>nothing here</html>").is_err());
        assert!(chapter_pages(r#"\"chapter_data\":{\"images\":[\"https://cdn.test/a.jpg\""#).is_err());
    }

    #[test]
    fn maps_address_extensions_onto_what_the_importer_reads() {
        assert_eq!(image_extension("https://cdn.test/a.jpg"), "jpg");
        // jpeg 与 jpg 是同一格式，归一后目录里不会出现两种写法。
        assert_eq!(image_extension("https://cdn.test/a.JPEG"), "jpg");
        assert_eq!(image_extension("https://cdn.test/a.png"), "png");
        assert_eq!(image_extension("https://cdn.test/a.WEBP?size=large"), "webp");
        // 导入层只认那四种，其余落到 jpg 而不是被静默丢掉。
        assert_eq!(image_extension("https://cdn.test/a.gif"), "jpg");
        assert_eq!(image_extension("https://cdn.test/a.avif"), "jpg");
        assert_eq!(image_extension("https://cdn.test/no-extension"), "jpg");
    }
}