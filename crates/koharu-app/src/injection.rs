//! 漫画级翻译资料的注入通道：把人工资料与上文渲染成一段散文，追加到翻译请求的附加说明末尾。
//!
//! **为什么只能走附加说明。** 翻译请求上有三个能承载内容的字段。语境条目在语义上其实最适合术语表——
//! 成对结构、离散条目，还自带「不要回译这些条目」的现成约束——但它的赋值点在翻译阶段内部，改它就是改
//! 流水线。附加说明是唯一一个既能表达内容、又能从外面换掉的通道
//! （`docs/reference/koharu-glossary-design.md` §3）。代价是内容必须渲染成散文，所以这一层负责把结构化
//! 资料翻译成模型读得懂的话。
//!
//! **段与顺序。** 漫画级翻译指导 → 术语表命中项 → 上文（章内 + 跨章）→ 用户的全局指导。逻辑是「一般到
//! 特殊，参考垫底」：先声明这本书的规矩，再给不可违反的译名，最后给可模仿的先例；全局指导排在后面，
//! 跨作品的口味不该压过作品自己的约定。这个顺序尚未实测，所以它由 [`Injection::order`] 决定而不是写死在
//! 渲染函数里。两段上文共用一个抬头与一份条目列表，因此它们在渲染上是一段，预算与丢弃顺序上仍是两段。
//!
//! **预算。** 各段共用一个字节预算，按「人工资料 → 章内上文 → 跨章上文」分配，丢弃顺序与优先级相反：
//! 章内上文从**最近**的一条开始丢，跨章上文从**最早**的一条开始丢。人工资料永远不丢——它是冷启动时唯一
//! 能生效的东西（`koharu-glossary-design.md` §2.1）。截断本身静默，但 [`Rendered`] 会报出丢了多少，界面
//! 要显示它，否则用户无法判断效果为什么不稳定。
//!
//! **服务商能力判断落在这里。** 作用域管线在构造之前已经读到了当前配置，其中的模型选择包含服务商，
//! 所以渲染期即可判断，零流水线改动（`koharu-glossary-design.md` §5）。不支持提示词的服务商会静默丢弃
//! 全部注入内容，所以必须在注入前关掉，并把这个事实报给界面。

use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use koharu_scene::Snapshot;

use crate::{commands::series::Glossary, glossary};

/// 上文段落的抬头。
///
/// 「不要翻译、不要输出」这句约束必须自带：系统提示词里对应的那句是随语境条目字段一起出现的，走附加
/// 说明通道时它不会出现（`koharu-glossary-design.md` §3.1）。漏掉它，模型会把先例里的译名当成待处理
/// 内容原样抄进译文。
const CONTEXT_HEADER: &str = "Earlier dialogue from this work, already translated. \
Reference only — do not translate it, do not output it, and do not comment on it. \
Reuse its wording and naming wherever the same words appear in the page you are given.";

/// 注入内容的字节预算。
///
/// 约 8 KiB，也就是两千多个 token。它要和「这一页要翻的文本」抢同一个上下文窗口，所以不能大到把正文挤
/// 出去。注意它只约束跨章上文：章内上文由翻译阶段逐页组装，自带一个页数上限（见
/// `koharu_pipeline::MAX_CONTEXT_PAGES`），而人工资料优先到不会被丢，所以过大时只能如实超出去。
const BUDGET: usize = 8 * 1024;

/// 跨章上文旁挂文件的前缀。文件名为 `context-<章序号>.json`，与漫画索引同目录。
const CONTEXT_FILE_PREFIX: &str = "context-";

/// 一条双语对照。
///
/// 上文存的是对照而不是术语：术语一致性由人工资料负责，上文负责的是「模型看到自己刚翻完的写法」
/// （`koharu-glossary-design.md` §1.2、§2.6）。
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub(crate) struct ContextEntry {
    pub source: String,
    pub translation: String,
}

impl ContextEntry {
    fn new(source: impl Into<String>, translation: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            translation: translation.into(),
        }
    }
}

/// 渲染一段注入内容需要的全部输入。
///
/// 全部按值收进来：这个函数不读文件也不读场景，所以它能在没有项目、场景与配置的环境里单测。
pub(crate) struct Injection {
    /// 本作特有的翻译风格约定。
    pub guidance: String,
    /// 术语表。命中判定用的 `haystack` 是整部漫画的原文，不是单页原文。
    pub glossary: Glossary,
    pub haystack: String,
    /// 上一章末尾的双语对照，按阅读顺序，最近的在后面。
    pub cross_chapter: Vec<ContextEntry>,
    /// 用户在设置里写的全局指导，排在最后作为跨作品的兜底。
    pub global: Option<String>,
    /// 段的顺序。`None` 用默认顺序；给它一份别的顺序就是换一种权重取舍，而不是改渲染逻辑。
    pub order: Option<Vec<Segment>>,
}

/// 注入内容的一段。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Segment {
    /// 本作手写的翻译指导。
    Guidance,
    /// 术语表命中的条目。
    Glossary,
    /// 上一章末尾的双语对照。
    CrossChapter,
    /// 用户全局指导。
    Global,
}

impl Segment {
    /// 默认顺序：一般到特殊，参考垫底。
    const DEFAULT_ORDER: [Segment; 4] = [
        Segment::Guidance,
        Segment::Glossary,
        Segment::CrossChapter,
        Segment::Global,
    ];
}

/// 渲染结果。
#[derive(Debug)]
pub(crate) struct Rendered {
    /// 要追加到附加说明末尾的文本。空串表示这一批不注入任何东西。
    pub text: String,
    /// 待译内容里命中的术语条数。
    pub matched: usize,
    /// 实际写进 `text` 的上文条数。
    pub injected: usize,
    /// 因为预算被丢掉的条文数。
    pub dropped: usize,
    /// 服务商不接受提示词，因此全部注入内容都不会生效。
    pub unsupported: bool,
}

/// 这一家服务商接不接受提示词。
///
/// 不接受的那几家只收固定参数，附加说明对它们完全无效，注入等于白费。判断是编译期穷举而不是配置：
/// 会不会走提示词是这一家的协议形状决定的，不是用户能选的。
pub(crate) fn accepts_instructions(provider: koharu_translator::Provider) -> bool {
    !matches!(
        provider,
        koharu_translator::Provider::DeepL
            | koharu_translator::Provider::GoogleCloudTranslation
            | koharu_translator::Provider::Caiyun
    )
}

/// 把各段拼成一段可以追加到附加说明里的散文。
pub(crate) fn render(injection: &Injection, provider: koharu_translator::Provider) -> Rendered {
    let matched = glossary::matched_entries(&injection.glossary, &injection.haystack);
    let terms = glossary::render(&injection.glossary, &matched);
    let total = injection.cross_chapter.len();
    if !accepts_instructions(provider) {
        return Rendered {
            text: String::new(),
            matched: matched.len(),
            injected: 0,
            dropped: total,
            unsupported: true,
        };
    }

    let mut cross_chapter = injection.cross_chapter.clone();
    trim_to_budget(&mut cross_chapter);

    let blocks = [
        (Segment::Guidance, one_paragraph(&injection.guidance)),
        (Segment::Glossary, terms),
        (
            Segment::CrossChapter,
            context_block(&cross_chapter),
        ),
        (
            Segment::Global,
            one_paragraph(injection.global.as_deref().unwrap_or_default()),
        ),
    ];
    let order = injection
        .order
        .clone()
        .unwrap_or_else(|| Segment::DEFAULT_ORDER.to_vec());
    let text = order
        .iter()
        .filter_map(|segment| {
            blocks
                .iter()
                .find(|(candidate, _)| candidate == segment)
                .map(|(_, block)| block.as_str())
        })
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    let injected = cross_chapter.len();
    Rendered {
        text,
        matched: matched.len(),
        injected,
        dropped: total - injected,
        unsupported: false,
    }
}

/// 跨章上文段落。抬头那句约束必须自带：系统提示词里对应的那句是随语境条目字段出现的，而跨章上文
/// 走的是附加说明通道（`koharu-glossary-design.md` §3.1）。
fn context_block(cross_chapter: &[ContextEntry]) -> String {
    if cross_chapter.is_empty() {
        return String::new();
    }
    let mut block = Vec::with_capacity(cross_chapter.len() + 1);
    block.push(CONTEXT_HEADER.to_owned());
    block.extend(cross_chapter.iter().map(|entry| {
        format!(
            "- {} → {}",
            one_paragraph(&entry.source),
            one_paragraph(&entry.translation)
        )
    }));
    block.join("\n")
}

/// 丢掉超出预算的条文，直到装得下。
///
/// 丢的是**最早**的一条：跨章上文保留最近的写法，因为称谓与语气的一致性看的是最近的。人工资料不
/// 在这里出现：它优先到不会被丢，所以上文全丢光还超预算就意味着人工资料本身太大，只能如实超出去。
///
/// 每一轮都重新量一次渲染后的长度，而不是拿「条目的字节和加一个常数」去估：预算的意义是「拼出来的
/// 文本不超过这个数」，估出来的数与真实长度差一个抬头就等于没约束。
fn trim_to_budget(cross_chapter: &mut Vec<ContextEntry>) {
    while context_block(cross_chapter).len() > BUDGET {
        if cross_chapter.is_empty() {
            return;
        }
        cross_chapter.remove(0);
    }
}

/// 待译内容里的全部原文，供术语命中判定。
///
/// 是整部漫画而不是单页：逐页匹配会漏掉那些在这一页没出现、但在别页出现的词条
/// （`koharu-glossary-design.md` §4.3）。因此这条要在批量开始前预扫描逐章收集一次。
pub(crate) fn source_text(snapshot: &Snapshot) -> Result<String> {
    let mut text = String::new();
    for page in snapshot.pages() {
        let Some(group) = page.text_group()? else {
            continue;
        };
        for layer in group.text_layers()? {
            let Some(source) = layer.content()?.source()? else {
                continue;
            };
            if source.text.value.trim().is_empty() {
                continue;
            }
            // 段之间只补换行、不折叠空白：待译全文的归一化刻意保留换行，跨气泡的假命中比漏命中坏得多。
            text.push_str(&source.text.value);
            text.push('\n');
        }
    }
    Ok(text)
}

/// 一个章节末尾 `pages` 页里已成对的原文与译文，用作下一章的跨章上文。
///
/// 抽取逻辑与翻译阶段的章内上文共用 `koharu_pipeline` 里的实现（`trailing_context`）：两者的判断
/// 规则必须一致，否则同一条对照在章内与跨章两个通道里会得到不同的取舍。
pub(crate) fn cross_chapter_context(snapshot: &Snapshot, pages: u32) -> Vec<ContextEntry> {
    koharu_pipeline::trailing_context(snapshot, pages)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| ContextEntry::new(entry.source, entry.translation))
        .collect()
}

/// 跨章上文旁挂文件的路径，按**目标章**的序号命名。
///
/// 按目标章而不是处理顺序命名：语义上的「前文」由作品结构决定，24 章的前文必定来自 23 章，即使这次批量
/// 只处理了 24 章（`koharu-glossary-design.md` §2.4）。
fn context_path(directory: &Path, seq: u32) -> std::path::PathBuf {
    directory.join(format!("{CONTEXT_FILE_PREFIX}{seq}.json"))
}

/// 读一部漫画为某章准备的跨章上文。文件不存在是合法状态，返回空。
///
/// 读回来是空的两级降级都不报错：上一章没有译文，或文件被手工删了。两项都安静退化成「没有跨章上文」，
/// 退回人工资料（`koharu-glossary-design.md` §2.5）。
pub(crate) fn load_cross_chapter(directory: &Path, seq: u32) -> Vec<ContextEntry> {
    let path = context_path(directory, seq);
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<ContextEntry>>(&contents) {
        Ok(entries) => entries,
        Err(error) => {
            // 旁挂文件是派生数据，坏掉只影响这一次的增强。删掉它比每次都失败好：下一次预扫描会重写。
            tracing::warn!(%error, path = %path.display(), "discarding an unreadable context file");
            let _ = fs::remove_file(&path);
            Vec::new()
        }
    }
}

/// 写一部漫画为某章准备的跨章上文。
pub(crate) fn save_cross_chapter(directory: &Path, seq: u32, entries: &[ContextEntry]) -> Result<()> {
    let path = context_path(directory, seq);
    if entries.is_empty() {
        // 上一章没有译文就是没有跨章上文。清掉旧文件，免得这一批跑在陈旧基线上。
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let contents = serde_json::to_string(entries).context("failed to encode the cross-chapter context")?;
    fs::write(&path, contents).with_context(|| format!("failed to write {}", path.display()))
}

/// 一段散文压成单段：空白折叠成单个空格，首尾空白去掉。
fn one_paragraph(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use koharu_scene::{
        At, Authored, Origin, PageDraft, Session, SourceText, TextContent, TextLayout,
        TextLayoutKind, Translation,
    };

    use crate::commands::series::{
        GlossaryEntry, GlossaryEntryId, GlossaryKind, GlossaryValueOrigin,
    };

    use super::*;

    fn entry(source: &str, translation: &str) -> GlossaryEntry {
        GlossaryEntry {
            id: GlossaryEntryId::new(),
            source: source.to_owned(),
            translation: Some(translation.to_owned()),
            kind: GlossaryKind::Term,
            enabled: true,
            note: String::new(),
            confidence: None,
            occurrence_count: 0,
            examples: Vec::new(),
            source_origin: GlossaryValueOrigin::User,
            translation_origin: Some(GlossaryValueOrigin::User),
            present_in_last_scan: true,
        }
    }

    fn glossary(entries: Vec<GlossaryEntry>) -> Glossary {
        Glossary {
            enabled: true,
            entries,
            ..Glossary::default()
        }
    }

    fn pair(source: &str, translation: &str) -> ContextEntry {
        ContextEntry::new(source, translation)
    }

    fn injection(entries: Vec<GlossaryEntry>, guidance: &str) -> Injection {
        Injection {
            guidance: guidance.to_owned(),
            glossary: glossary(entries),
            haystack: "アリスは笑った".to_owned(),
            cross_chapter: Vec::new(),
            global: None,
            order: None,
        }
    }

    /// 一页一页地建一个项目，每页一条文本。译文为 `None` 表示这一页还没翻。
    async fn project(pages: &[(&str, Option<&str>)]) -> Snapshot {
        let mut session = Session::memory().await.expect("memory session");
        let mut edit = session.snapshot().edit();
        for (source, translated) in pages {
            let page = edit
                .add_page(PageDraft::new(*source, 100.0, 100.0), At::End)
                .expect("add page");
            let content = edit.add_text_content(page, At::End).expect("add content");
            edit.set(
                content,
                &TextContent {
                    origin: Origin::User,
                },
            )
            .expect("set content");
            edit.set(
                content,
                &SourceText {
                    text: Authored::user((*source).to_owned()),
                    language: None,
                },
            )
            .expect("set source");
            edit.add_text_layer(
                page,
                At::End,
                content,
                &TextLayout {
                    origin: Origin::User,
                    kind: TextLayoutKind::Paragraph,
                    angle_degrees: None,
                },
            )
            .expect("add text layer");
            if let Some(translated) = translated {
                edit.set(
                    content,
                    &Translation {
                        text: Authored::user((*translated).to_owned()),
                        language: None,
                    },
                )
                .expect("set translation");
            }
        }
        session
            .commit(edit.finish().expect("finish"))
            .await
            .expect("commit")
            .snapshot
    }

    #[test]
    fn a_provider_without_prompts_injects_nothing_and_says_so() {
        let mut built = injection(vec![entry("アリス", "Alice")], "keep it short");
        built.cross_chapter.push(pair("こんにちは", "Hello"));
        for provider in [
            koharu_translator::Provider::DeepL,
            koharu_translator::Provider::GoogleCloudTranslation,
            koharu_translator::Provider::Caiyun,
        ] {
            let rendered = render(&built, provider);
            assert!(rendered.text.is_empty(), "{provider} drops the instructions");
            assert!(rendered.unsupported, "{provider} must be reported");
            assert_eq!(rendered.matched, 1, "the hit count is still worth reporting");
            assert_eq!(rendered.dropped, 1, "and so is what was lost");
        }
        assert!(!render(&built, koharu_translator::Provider::Local).unsupported);
    }

    #[test]
    fn the_segments_appear_in_the_declared_order() {
        let mut built = injection(vec![entry("アリス", "Alice")], "Speak casually.");
        built.cross_chapter.push(pair("やっほい", "Yo"));
        built.global = Some("Prefer British spellings.".to_owned());

        let text = render(&built, koharu_translator::Provider::Local).text;
        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle} in\n{text}"));
        assert!(at("Speak casually.") < at("アリス"), "guidance before terminology");
        assert!(at("アリス") < at(CONTEXT_HEADER), "terminology before precedent");
        assert!(at(CONTEXT_HEADER) < at("Prefer British spellings."), "precedent before taste");
        assert!(text.contains("- やっほい → Yo"), "{text}");
    }

    #[test]
    fn an_order_override_changes_the_weights_not_the_content() {
        let mut built = injection(vec![entry("アリス", "Alice")], "Speak casually.");
        built.cross_chapter.push(pair("やっほい", "Yo"));
        let straight = render(&built, koharu_translator::Provider::Local).text;
        assert!(straight.find("Speak casually.") < straight.find("アリス"));

        built.order = Some(vec![
            Segment::Glossary,
            Segment::CrossChapter,
            Segment::Guidance,
            Segment::Global,
        ]);
        let reordered = render(&built, koharu_translator::Provider::Local).text;
        assert_ne!(straight, reordered);
        assert!(reordered.find("アリス") < reordered.find(CONTEXT_HEADER));
        assert!(reordered.find(CONTEXT_HEADER) < reordered.find("Speak casually."));
    }

    #[test]
    fn nothing_to_inject_produces_nothing() {
        let empty = render(
            &injection(Vec::new(), "  "),
            koharu_translator::Provider::Local,
        );
        assert_eq!(empty.text, "", "a blank guidance and an empty glossary");
        assert_eq!(empty.matched, 0);

        let mut missed = injection(vec![entry("アリス", "Alice")], "");
        missed.haystack = "無関係".to_owned();
        assert_eq!(render(&missed, koharu_translator::Provider::Local).text, "");
    }

    #[test]
    fn the_budget_drops_the_oldest_cross_chapter_pair() {
        // 丢的是最早的一条：跨章上文保留最近的写法，因为称谓与语气的一致性看的是最近的。
        let mut built = injection(Vec::new(), "");
        built.cross_chapter = (0..200)
            .map(|index| pair(&format!("pre{index}"), &"p".repeat(64)))
            .collect();

        let rendered = render(&built, koharu_translator::Provider::Local);
        assert!(rendered.text.len() <= BUDGET, "must fit the budget");
        assert_eq!(rendered.injected + rendered.dropped, 200);
        assert!(!rendered.text.contains("pre0"), "the earliest pair goes");
        assert!(rendered.text.contains("pre199"), "the most recent one stays");
        assert_eq!(
            built.cross_chapter.len(),
            200,
            "the caller's copy is never mutated"
        );
    }

    #[test]
    fn manual_assets_survive_an_over_budget_table() {
        let entries = (0..400)
            .map(|index| entry(&format!("ワープ{index}"), &"Warp".repeat(16)))
            .collect();
        let mut built = injection(entries, &"g".repeat(BUDGET));
        built.haystack = (0..400).map(|index| format!("ワープ{index} ")).collect();

        let rendered = render(&built, koharu_translator::Provider::Local);
        assert!(
            rendered.text.len() > BUDGET,
            "manual assets are never dropped, so they are not bounded by it"
        );
        assert_eq!(rendered.matched, 400);
        assert_eq!(
            rendered.injected, 0,
            "an over-budget table leaves no room for precedent"
        );
    }

    #[tokio::test]
    async fn source_text_collects_every_page_in_reading_order() {
        let snapshot = project(&[("first", Some("一つ")), ("second", None)]).await;
        assert_eq!(source_text(&snapshot).unwrap(), "first\nsecond\n");
    }

    #[tokio::test]
    async fn the_cross_chapter_context_takes_the_tail_of_the_previous_chapter() {
        let snapshot = project(&[
            ("oldest", Some("一番古く")),
            ("middle", None),
            ("newest", Some("一番新しい")),
        ])
        .await;
        let context = cross_chapter_context(&snapshot, 2);
        assert_eq!(
            context
                .iter()
                .map(|entry| entry.source.as_str())
                .collect::<Vec<_>>(),
            vec!["newest"],
            "a page without a translation contributes nothing"
        );
        assert!(cross_chapter_context(&snapshot, 0).is_empty());
    }

    #[test]
    fn the_cross_chapter_file_is_named_after_the_target_chapter() {
        let directory = std::env::temp_dir().join(format!(
            "koharu-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create fixture");

        assert!(load_cross_chapter(&directory, 24).is_empty(), "a missing file");
        save_cross_chapter(&directory, 24, &[pair("前", "before")]).expect("save");
        assert!(directory.join("context-24.json").is_file());
        assert_eq!(load_cross_chapter(&directory, 24), vec![pair("前", "before")]);
        // 序号 23 读的是自己的文件，与 24 无关。
        assert!(load_cross_chapter(&directory, 23).is_empty());

        // 空就是没有跨章上文，清掉旧文件而不是留一份陈旧基线。
        save_cross_chapter(&directory, 24, &[]).expect("clear");
        assert!(!directory.join("context-24.json").exists());

        fs::write(directory.join("context-24.json"), b"{ not json").expect("write broken");
        assert!(load_cross_chapter(&directory, 24).is_empty(), "a broken file degrades quietly");
        assert!(!directory.join("context-24.json").exists(), "and is discarded");

        fs::remove_dir_all(&directory).expect("remove fixture");
    }
}
