//! OmegaScans（`omegascans.org`）协议。
//!
//! 地址模板与页面形状的调研记录在 `docs/reference/omegascans-protocol.md`，含每条的来源与实测时间。
//! 那边是参考输入而不是规格：站点形态变了改这里，并重新对活站验证，不要照着文档改。
//!
//! 与上游那份 Python 实现的有意分歧都写在各自那行代码上，最要紧的一条是本模块**不做**重试与
//! 并发：那是编排层按站点承受度决定的事，协议层只负责一次请求一次结果。

mod naming;
mod parse;

pub use naming::{chapter_directory_name, chapter_number, page_file_name, parse_series_url};
pub use parse::{ChapterPages, image_extension};

use std::collections::HashSet;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// 站点页面。
const SITE: &str = "https://omegascans.org";
/// 站点 JSON 接口。
const API: &str = "https://api.omegascans.org";

/// `chapter/query` 的分页大小。这是站点自己的页大小，翻页终止以响应里的页数为准。
const CHAPTER_PAGE_SIZE: u32 = 12;

/// 短于这个长度的响应一定不是页面，是站点把我们甩到了别处。
const MIN_PAGE_BYTES: usize = 1024;

/// 短于这个长度的响应一定不是图片。
const MIN_IMAGE_BYTES: usize = 1024;

/// 站点要求的 UA。
///
/// **有意偏离共享 HTTP 策略**：那边的 UA 是 `koharu/<版本>`，用途是标识自己；
/// 这里必须装作浏览器，否则站点直接拒掉。超时也自己定：共享策略的读超时是按模型下载那种
/// 大文件定的，而一个阅读页只有几十 KB。
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36";

/// 一个作品。
#[derive(Clone, Debug)]
pub struct RemoteSeries {
    /// 站内作品号，章节列表按它查。
    pub id: u64,
    pub title: String,
}

/// 一个作品的一章。
#[derive(Clone, Debug)]
pub struct RemoteChapter {
    /// 站点自己给的名字，也是本地这一章的身份。
    pub name: String,
    /// 阅读页地址的最后一段。
    pub slug: String,
}

/// OmegaScans 客户端。
///
/// 传输由注入的 [`reqwest::Client`] 决定，调用方因此能统一管重试与日志。
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
}

impl Client {
    /// 按站点要求自建一个客户端。
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(60))
            .build()
            .context("failed to build the OmegaScans HTTP client")?;
        Ok(Self { http })
    }

    /// 用调用方给的客户端。
    pub fn with_client(http: reqwest::Client) -> Self {
        Self { http }
    }

    /// 一个作品的信息。slug 由调用方从用户粘贴的地址里取。
    pub async fn series(&self, slug: &str) -> Result<RemoteSeries> {
        let body = self.text(&format!("{API}/series/{slug}")).await?;
        parse_series(&body)
    }

    /// 一个作品的全部章节，按章号升序。
    ///
    /// 每翻一页都重算已见集合而不是信任分页边界：站点更新期间会让相邻两页短暂重叠，
    /// 而重复的章会在落盘时撞上同名目录。
    pub async fn chapters(&self, series_id: u64) -> Result<Vec<RemoteChapter>> {
        let mut collected: Vec<RemoteChapter> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut page = 1;
        loop {
            let body = self
                .text(&format!(
                    "{API}/chapter/query?page={page}&limit={CHAPTER_PAGE_SIZE}&series_id={series_id}"
                ))
                .await?;
            let (listed, last_page) = parse_chapter_page(&body)?;
            let finished = listed.is_empty() || page >= last_page;
            collected.extend(listed.into_iter().filter(|entry| seen.insert(entry.slug.clone())));
            if finished {
                break;
            }
            page += 1;
        }

        // 章号升序。站点按更新时间倒序给，而缺失清单要按阅读顺序呈现，下载也先补更早的那几章。
        collected.sort_by(|left, right| {
            let order = |chapter: &RemoteChapter| {
                chapter_number(&chapter.name).unwrap_or(f64::NEG_INFINITY)
            };
            order(left)
                .partial_cmp(&order(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(collected)
    }

    /// 一章的图片清单。清单为空说明这一章尚未公开，不是失败。
    pub async fn chapter_pages(&self, series_slug: &str, chapter_slug: &str) -> Result<ChapterPages> {
        let url = format!("{SITE}/series/{series_slug}/{chapter_slug}");
        let html = self.text(&url).await?;
        // 体积下限让「200 但内容是错误页」在这里就失败，而不是变成一句「找不到图片清单」，
        // 后者会让人以为是站点 markup 变了。
        if html.len() < MIN_PAGE_BYTES {
            bail!(
                "the chapter page was {} bytes, which is an error page rather than a chapter page",
                html.len()
            );
        }
        parse::chapter_pages(&html)
    }

    /// 取一张图片的字节。
    ///
    /// 站点偶尔把错误页按图片返回，所以过小的结果直接判失败：让编排层去重试，
    /// 而不是把一个坏页写进章里、让它在后面的检测阶段才炸。
    pub async fn image(&self, url: &str) -> Result<Vec<u8>> {
        let bytes = self
            .get(url)
            .await?
            .bytes()
            .await
            .with_context(|| format!("the image at {url} ended early"))?;
        if bytes.len() < MIN_IMAGE_BYTES {
            bail!(
                "the image at {url} was {} bytes, which is an error page rather than a page image",
                bytes.len()
            );
        }
        Ok(bytes.to_vec())
    }

    async fn get(&self, url: &str) -> Result<reqwest::Response> {
        self.http
            .get(url)
            .send()
            .await
            .with_context(|| format!("{url} could not be reached"))?
            .error_for_status()
            .with_context(|| format!("{url} answered with an error status"))
    }

    async fn text(&self, url: &str) -> Result<String> {
        self.get(url)
            .await?
            .text()
            .await
            .with_context(|| format!("{url} sent a body that could not be read"))
    }
}

/// 作品接口的响应。
#[derive(Deserialize)]
struct SeriesResponse {
    // 字段一律声明成可选，只为把「缺 id」变成一句能读懂的错误，而不是 serde 的字段名。
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    title: Option<String>,
}

/// `chapter/query` 的响应。
#[derive(Deserialize)]
struct ChapterPageResponse {
    #[serde(default)]
    data: Option<Vec<ChapterEntry>>,
    #[serde(default)]
    meta: Option<PageMeta>,
}

#[derive(Deserialize)]
struct PageMeta {
    #[serde(default)]
    last_page: Option<u64>,
}

#[derive(Deserialize)]
struct ChapterEntry {
    #[serde(default)]
    chapter_name: Option<String>,
    #[serde(default)]
    chapter_slug: Option<String>,
}

/// 解析作品接口的响应。
///
/// 站点换了形状要能一眼看出来，所以这里不靠 serde 的字段缺失报错，而是自己给一句说人话的。
fn parse_series(body: &str) -> Result<RemoteSeries> {
    let response: SeriesResponse = serde_json::from_str(body)
        .context("the series endpoint returned a body that is not the expected JSON")?;
    Ok(RemoteSeries {
        id: response
            .id
            .context("the series endpoint returned no id, so the API shape likely changed")?,
        title: response.title.unwrap_or_default(),
    })
}

/// 解析 `chapter/query` 的一页，连同翻页终止所需的页数。
fn parse_chapter_page(body: &str) -> Result<(Vec<RemoteChapter>, u64)> {
    let response: ChapterPageResponse = serde_json::from_str(body)
        .context("the chapter query returned a body that is not the expected JSON")?;
    // 缺页数就只剩「一直翻到空页」这一条路，而那在站点改形状时会变成一个不知道何时停的循环。
    let last_page = response
        .meta
        .and_then(|meta| meta.last_page)
        .context("the chapter query returned no last_page, so the API shape likely changed")?;
    // 名字或地址缺一个就整条不可用：没有名字落不了盘，没有地址读不到阅读页。
    let chapters = response
        .data
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            Some(RemoteChapter {
                name: entry.chapter_name?,
                slug: entry.chapter_slug?,
            })
        })
        .collect();
    Ok((chapters, last_page))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_series_body() {
        let series =
            parse_series(r#"{"id":758,"title":"Demo Title","cover":"x.jpg"}"#).expect("should parse");
        assert_eq!(series.id, 758);
        assert_eq!(series.title, "Demo Title");
    }

    #[test]
    fn refuses_a_series_body_it_did_not_understand() {
        // 换形状时要说清楚是形状变了，而不是 serde 抛一个字段名。
        assert!(parse_series(r#"{"title":"Demo Title"}"#).is_err());
        assert!(parse_series(r#"["758"]"#).is_err());
        assert!(parse_series("<html>404</html>").is_err());
    }

    #[test]
    fn reads_a_chapter_page_body() {
        let (chapters, last_page) = parse_chapter_page(
            r#"{"data":[{"chapter_name":"Chapter 36","chapter_slug":"chapter-36","price":"0"},
                        {"chapter_name":"Chapter 35","chapter_slug":"chapter-35"}],
                "meta":{"last_page":4,"current_page":1}}"#,
        )
        .expect("the chapter page should parse");
        assert_eq!(last_page, 4);
        assert_eq!(chapters.len(), 2);
        assert_eq!(chapters[0].slug, "chapter-36");
    }

    #[test]
    fn drops_entries_it_cannot_act_on() {
        // 名字或地址缺一个就整条不可用，但同页的其他条目要保住。
        let (chapters, last_page) = parse_chapter_page(
            r#"{"data":[{"chapter_name":"Chapter 36"},
                        {"chapter_slug":"chapter-35"},
                        {"chapter_name":"Chapter 34","chapter_slug":"chapter-34"}],
                "meta":{"last_page":2}}"#,
        )
        .expect("a partial entry should not fail the page");
        assert_eq!(last_page, 2);
        assert_eq!(
            chapters
                .iter()
                .map(|chapter| chapter.slug.as_str())
                .collect::<Vec<_>>(),
            ["chapter-34"]
        );
    }

    #[test]
    fn refuses_a_chapter_body_without_a_page_count() {
        // 没有页数就没有翻页终止条件，宁可报错也不要无声地翻下去。
        assert!(parse_chapter_page(r#"{"data":[]}"#).is_err());
        assert!(parse_chapter_page(r#"{"meta":{}}"#).is_err());
    }

    #[test]
    fn reads_an_empty_page_as_a_drained_list() {
        let (chapters, last_page) = parse_chapter_page(r#"{"data":[],"meta":{"last_page":1}}"#)
            .expect("an empty page parses");
        assert!(chapters.is_empty());
        assert_eq!(last_page, 1);
    }
}