//! 站点侧的命名规则：作品地址怎么读、章名怎么变成目录名、页面文件叫什么。
//!
//! 归一化之后的名字同时是两样东西：临时目录名，以及 `SeriesChapter.title` 的来源。
//! 后者正是增量判定比对的那个键。所以这里的每个函数都必须只依赖站点给的名字，
//! 不能掺任何本地状态：同一个章名过两次要得到同一个字符串，否则会把已下载的章当成新的再下一次。

use anyhow::{Context as _, Result};

/// Windows 不允许出现在文件名里的字符。
///
/// 站点标题不受我们的约束，而这个字符��要真的拿去 `create_dir`，所以在源头换掉比在失败后补救便宜。
const ILLEGAL: [char; 9] = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];

/// 从用户粘贴的地址里取作品 slug。
///
/// 收三种写法：完整作品页、缺协议头的地址、裸 slug。章节页地址取 `/series/` 后的第一段，
/// 因为用户多半是从阅读页复制地址栏的，而把整条地址当 slug 会静默拉到别的作品上去。
///
/// 唯一的歧义是「既没有协议头也没有 `series/` 前缀」：这时只能当裸 slug 处理。站点的 slug 是
/// 短横线小写、带点的极少，所以「首段含点即域名」这条判据在实践中够用。
pub fn parse_series_url(input: &str) -> Result<&str> {
    let trimmed = input.trim().trim_end_matches('/');
    let without_scheme = match trimmed.split_once("://") {
        Some((_, rest)) => rest,
        None => trimmed,
    };

    let mut segments: Vec<&str> = without_scheme
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    // 省略协议头是复制地址栏时的常态，`omegascans.org/series/<slug>` 得认。
    if segments.first().is_some_and(|head| head.contains('.')) {
        segments.remove(0);
    }
    if segments.first() == Some(&"series") {
        segments.remove(0);
    }

    segments
        .first()
        .copied()
        .filter(|slug| !slug.is_empty())
        .context("the address carries no series slug; paste the series page address")
}

/// 章名里的第一个数字：`Chapter 24.5` → 24.5，`Prologue` → `None`。
///
/// 取第一个而不是最后一段：`Chapter 24.5` 只有一个数字，`24.5 Extra` 这种站点自造标题按首段
/// 理解仍然对。没有数字的章排在最前面，调用方拿 `NEG_INFINITY` 兜底。
pub fn chapter_number(name: &str) -> Option<f64> {
    let bytes = name.as_bytes();
    let start = bytes.iter().position(u8::is_ascii_digit)?;
    let mut end = start;
    let mut seen_point = false;
    while end < bytes.len() {
        match bytes[end] {
            b'0'..=b'9' => end += 1,
            // 只认一个小数点，否则 `1.2.3` 会被截成非法数字。
            b'.' if !seen_point => {
                seen_point = true;
                end += 1;
            }
            _ => break,
        }
    }

    // `Chapter 24.` 的尾巴那个点会让 `parse` 失败，而语义上就是 24。
    name.get(start..end)?.trim_end_matches('.').parse().ok()
}

/// 章名 → 目录名。
///
/// 补 `Chapter ` 前缀不是为了对齐 koharu 的章项目目录（那个叫 `Chapter <序号>`，由序号生成），
/// 而是为了和手动导入对齐：用户从生肉站点手动存下来的文件夹也叫 `Chapter 36`，两边归一之后
/// 得到同一个 `title`，增量判定才不会把同一章当成两章。
pub fn chapter_directory_name(name: &str) -> String {
    let trimmed = name.trim();
    // `Chapter 36` / `chapter 36` / `Chapter36` 都不再补前缀，最后那个要靠后面不是字母数字来区分。
    let named = match trimmed.get(..7) {
        Some(head) if head.eq_ignore_ascii_case("chapter") => {
            let rest = &trimmed[7..];
            if rest.starts_with(char::is_alphanumeric) {
                format!("Chapter {trimmed}")
            } else {
                trimmed.to_owned()
            }
        }
        _ => format!("Chapter {trimmed}"),
    };

    named
        .chars()
        .map(|character| {
            if ILLEGAL.contains(&character) {
                '-'
            } else {
                character
            }
        })
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// 章目录里一页图片的文件名。
///
/// 序号取图片在站点清单里的位置而不是「成功下载到的第几张」——失败的那几张因此留下空洞，
/// 重新下载时页序不会整体前移，站点改图也不会让前后页错位。
///
/// `total` 只用来决定补零位数：页数过百时 `001.jpg` 与 `0001.jpg` 会并存，
/// 而 `collect_importable` 的自然排序读不出这个差别。
pub fn page_file_name(index: usize, total: usize, extension: &str) -> String {
    let width = total.to_string().len().max(3);
    format!("{:0width$}.{extension}", index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_slug_out_of_every_shape_a_paste_can_take() {
        let expected = "love-quest";
        let accepted = [
            "https://omegascans.org/series/love-quest",
            "https://omegascans.org/series/love-quest/",
            "https://omegascans.org/series/love-quest/chapter-36",
            "omegascans.org/series/love-quest",
            "https://omegascans.org/love-quest",
            "love-quest",
            "  love-quest  ",
        ];
        for input in accepted {
            assert_eq!(parse_series_url(input).unwrap(), expected, "{input:?}");
        }
    }

    #[test]
    fn rejects_an_address_without_a_slug() {
        for input in ["", "   ", "/", "https://omegascans.org/", "omegascans.org"] {
            assert!(parse_series_url(input).is_err(), "{input:?}");
        }
    }

    #[test]
    fn reads_the_first_number_out_of_a_chapter_name() {
        assert_eq!(chapter_number("Chapter 24.5"), Some(24.5));
        assert_eq!(chapter_number("Chapter 35"), Some(35.0));
        assert_eq!(chapter_number("36.5"), Some(36.5));
        assert_eq!(chapter_number("24.5 Extra"), Some(24.5));
        // 尾随的点不能把解析带崩，语义上就是 24。
        assert_eq!(chapter_number("Chapter 24."), Some(24.0));
        assert_eq!(chapter_number("Chapter 1.2.3"), Some(1.2));
        assert_eq!(chapter_number("Prologue"), None);
        assert_eq!(chapter_number(""), None);
    }

    #[test]
    fn reads_a_number_past_multibyte_text() {
        // 站点标题里有非 ASCII 时，切片必须落在字符边界上而不是panic。
        assert_eq!(chapter_number("第 12 话"), Some(12.0));
    }

    #[test]
    fn guarantees_the_chapter_prefix_without_doubling_it() {
        assert_eq!(chapter_directory_name("Chapter 36"), "Chapter 36");
        assert_eq!(chapter_directory_name("36"), "Chapter 36");
        assert_eq!(chapter_directory_name("36.5"), "Chapter 36.5");
        assert_eq!(chapter_directory_name("chapter 12"), "chapter 12");
        // 没有词边界说明「Chapter」是标题的一部分，不算已经带前缀。
        assert_eq!(chapter_directory_name("Chapter"), "Chapter");
        assert_eq!(chapter_directory_name("Chapter36"), "Chapter Chapter36");
        assert_eq!(chapter_directory_name("  Chapter 2  "), "Chapter 2");
    }

    #[test]
    fn replaces_what_a_directory_name_cannot_carry() {
        assert_eq!(
            chapter_directory_name("a<b>:\"c/d\\e|f?g*h"),
            "Chapter a-b---c-d-e-f-g-h"
        );
        assert_eq!(chapter_directory_name("Chapter 36 "), "Chapter 36");
    }

    #[test]
    fn pads_page_names_so_a_hundred_page_chapter_still_sorts() {
        assert_eq!(page_file_name(0, 7, "jpg"), "001.jpg");
        assert_eq!(page_file_name(6, 7, "jpg"), "007.jpg");
        // 位数按整章页数走，不按单张页码走。
        assert_eq!(page_file_name(0, 120, "jpg"), "001.jpg");
        assert_eq!(page_file_name(119, 120, "jpg"), "120.jpg");
    }
}