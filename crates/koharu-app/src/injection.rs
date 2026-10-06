//! 漫画级翻译资料的注入通道：把人工资料渲染成一段散文，追加到翻译请求的附加说明末尾。
//!
//! **为什么只能走附加说明。** 翻译请求上有三个能承载内容的字段。语境条目在语义上其实最适合术语表——
//! 成对结构、离散条目，还自带「不要回译这些条目」的现成约束——但它的赋值点在翻译阶段内部，改它就是改
//! 流水线。附加说明是唯一一个既能表达内容、又能从外面换掉的通道
//! （`docs/reference/koharu-glossary-design.md` §3）。代价是内容必须渲染成散文，所以这一层负责把结构化
//! 资料翻译成模型读得懂的话。
//!
//! **段与顺序。** 漫画级翻译指导 → 术语表命中项 → 用户的全局指导。逻辑是「一般到特殊，参考垫底」：先
//! 声明这本书的规矩，再给不可违反的译名；全局指导排在后面，跨作品的口味不该压过作品自己的约定。这个
//! 顺序尚未实测，所以它由 [`Injection::order`] 决定而不是写死在渲染函数里。
//!
//! **上文不走这一层。** 上文曾经以散文段落的形式挂在附加说明末尾，代价有两个：整章每一页都重复看着
//! 同一批先例，而那一段与翻译阶段自己取的章内部分各按 `context_pages` 算一遍，总量最多到两倍页。现在
//! 上文整个搬进语境条目通道，由翻译阶段就地取一个**跨章连续**的窗口，逐页不同、总量恒为
//! `context_pages` 页（见 `koharu_pipeline::preceding_context`）。这一层因此不再有字节预算：人工资料是
//! 用户手写的、不会被丢，上文的总量由页数上限约束。
//!
//! **服务商能力判断落在这里。** 作用域管线在构造之前已经读到了当前配置，其中的模型选择包含服务商，
//! 所以渲染期即可判断，零流水线改动（`koharu-glossary-design.md` §5）。不支持提示词的服务商会静默丢弃
//! 全部注入内容，所以必须在注入前关掉，并把这个事实报给界面。

use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use koharu_scene::Snapshot;
use koharu_translator::TranslationContext;

use crate::{commands::series::Glossary, glossary};

/// 上文旁挂文件的前缀。文件名为 `context-<章序号>.json`，与漫画索引同目录。
const CONTEXT_FILE_PREFIX: &str = "context-";

/// 渲染一段注入内容需要的全部输入。
///
/// 全部按值收进来：这个函数不读文件也不读场景，所以它能在没有项目、场景与配置的环境里单测。
pub(crate) struct Injection {
    /// 本作特有的翻译风格约定。
    pub guidance: String,
    /// 术语表。命中判定用的 `haystack` 是整部漫画的原文，不是单页原文。
    pub glossary: Glossary,
    pub haystack: String,
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
    /// 用户全局指导。
    Global,
}

impl Segment {
    /// 默认顺序：一般到特殊，参考垫底。
    const DEFAULT_ORDER: [Segment; 3] = [Segment::Guidance, Segment::Glossary, Segment::Global];
}

/// 渲染结果。
#[derive(Debug)]
pub(crate) struct Rendered {
    /// 要写进附加说明的文本。空串表示这一批不注入任何东西。
    pub text: String,
    /// 待译内容里命中的术语条数。
    pub matched: usize,
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
    if !accepts_instructions(provider) {
        return Rendered {
            text: String::new(),
            matched: matched.len(),
            unsupported: true,
        };
    }

    let blocks = [
        (Segment::Guidance, one_paragraph(&injection.guidance)),
        (
            Segment::Glossary,
            glossary::render(&injection.glossary, &matched),
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

    Rendered {
        text,
        matched: matched.len(),
        unsupported: false,
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

/// 一个章节末尾 `pages` 页里已成对的原文与译文，按页分桶，用作下一章上文窗口的章外那一段。
///
/// 抽取直接用 `koharu_pipeline::trailing_pages`：翻译阶段取窗口时用的是同一个函数，两处对「哪一页算
/// 进来」的判断必须一致，否则同一条对照在章内与章外会得到不同的取舍。
pub(crate) fn prior_chapter_context(snapshot: &Snapshot, pages: u32) -> Vec<Vec<TranslationContext>> {
    koharu_pipeline::trailing_pages(snapshot, pages).unwrap_or_default()
}

/// 上文旁挂文件的路径，按**目标章**的序号命名。
///
/// 按目标章而不是处理顺序命名：语义上的「前文」由作品结构决定，24 章的前文必定来自 23 章，即使这次批量
/// 只处理了 24 章（`koharu-glossary-design.md` §2.4）。
fn context_path(directory: &Path, seq: u32) -> std::path::PathBuf {
    directory.join(format!("{CONTEXT_FILE_PREFIX}{seq}.json"))
}

/// 读一部漫画为某章准备的上文。文件不存在是合法状态，返回空。
///
/// 读回来是空的两级降级都不报错：上一章没有译文，或文件被手工删了。两项都安静退化成「没有上文」，
/// 退回人工资料（`koharu-glossary-design.md` §2.5）。
pub(crate) fn load_prior_context(directory: &Path, seq: u32) -> Vec<Vec<TranslationContext>> {
    let path = context_path(directory, seq);
    let Ok(contents) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<Vec<TranslationContext>>>(&contents) {
        Ok(pages) => pages,
        Err(error) => {
            // 旁挂文件是派生数据，坏掉只影响这一次的增强。删掉它比每次都失败好：下一次预扫描会重写。
            tracing::warn!(%error, path = %path.display(), "discarding an unreadable context file");
            let _ = fs::remove_file(&path);
            Vec::new()
        }
    }
}

/// 写一部漫画为某章准备的上文。
///
/// 空就是上一章没有译文，清掉旧文件，免得这一批跑在陈旧基线上。
pub(crate) fn save_prior_context(
    directory: &Path,
    seq: u32,
    pages: &[Vec<TranslationContext>],
) -> Result<()> {
    let path = context_path(directory, seq);
    if pages.is_empty() {
        let _ = fs::remove_file(&path);
        return Ok(());
    }
    let contents = serde_json::to_string(pages).context("failed to encode the prior context")?;
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

    fn pair(source: &str, translation: &str) -> TranslationContext {
        TranslationContext {
            source: source.to_owned(),
            translation: translation.to_owned(),
        }
    }

    fn injection(entries: Vec<GlossaryEntry>, guidance: &str) -> Injection {
        Injection {
            guidance: guidance.to_owned(),
            glossary: glossary(entries),
            haystack: "アリスは笑った".to_owned(),
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
        let built = injection(vec![entry("アリス", "Alice")], "keep it short");
        for provider in [
            koharu_translator::Provider::DeepL,
            koharu_translator::Provider::GoogleCloudTranslation,
            koharu_translator::Provider::Caiyun,
        ] {
            let rendered = render(&built, provider);
            assert!(rendered.text.is_empty(), "{provider} drops the instructions");
            assert!(rendered.unsupported, "{provider} must be reported");
            assert_eq!(rendered.matched, 1, "the hit count is still worth reporting");
        }
        assert!(!render(&built, koharu_translator::Provider::Local).unsupported);
    }

    #[test]
    fn the_segments_appear_in_the_declared_order() {
        let mut built = injection(vec![entry("アリス", "Alice")], "Speak casually.");
        built.global = Some("Prefer British spellings.".to_owned());

        let text = render(&built, koharu_translator::Provider::Local).text;
        let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle} in\n{text}"));
        assert!(at("Speak casually.") < at("アリス"), "guidance before terminology");
        assert!(
            at("アリス") < at("Prefer British spellings."),
            "terminology before taste"
        );
    }

    #[test]
    fn an_order_override_changes_the_weights_not_the_content() {
        let mut built = injection(vec![entry("アリス", "Alice")], "Speak casually.");
        built.global = Some("Prefer British spellings.".to_owned());
        let straight = render(&built, koharu_translator::Provider::Local).text;
        assert!(straight.find("Speak casually.") < straight.find("アリス"));

        built.order = Some(vec![Segment::Glossary, Segment::Global, Segment::Guidance]);
        let reordered = render(&built, koharu_translator::Provider::Local).text;
        assert_ne!(straight, reordered);
        assert!(reordered.find("アリス") < reordered.find("Prefer British spellings."));
        assert!(reordered.find("Prefer British spellings.") < reordered.find("Speak casually."));
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
    fn an_oversized_table_is_still_injected_whole() {
        // 人工资料是用户手写的，一个字都不能丢。这里没有预算，因此也没有「被丢掉多少」要报给界面。
        let entries = (0..400)
            .map(|index| entry(&format!("ワープ{index}"), &"Warp".repeat(16)))
            .collect();
        let mut built = injection(entries, "");
        built.haystack = (0..400).map(|index| format!("ワープ{index} ")).collect();

        let rendered = render(&built, koharu_translator::Provider::Local);
        assert_eq!(rendered.matched, 400);
        for index in 0..400 {
            assert!(
                rendered.text.contains(&format!("ワープ{index} → ")),
                "entry {index} is missing"
            );
        }
    }

    #[tokio::test]
    async fn source_text_collects_every_page_in_reading_order() {
        let snapshot = project(&[("first", Some("一つ")), ("second", None)]).await;
        assert_eq!(source_text(&snapshot).unwrap(), "first\nsecond\n");
    }

    #[tokio::test]
    async fn the_prior_context_takes_the_tail_page_by_page() {
        let snapshot = project(&[
            ("oldest", Some("一番古く")),
            ("middle", None),
            ("newest", Some("一番新しい")),
        ])
        .await;
        let sources = |pages: Vec<Vec<TranslationContext>>| {
            pages
                .iter()
                .map(|pairs| {
                    pairs
                        .iter()
                        .map(|entry| entry.source.clone())
                        .collect::<Vec<String>>()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            sources(prior_chapter_context(&snapshot, 2)),
            vec![vec![], vec!["newest"]],
            "a page without a translation keeps its slot and contributes nothing"
        );
        assert!(prior_chapter_context(&snapshot, 0).is_empty());
    }

    #[test]
    fn the_context_file_is_named_after_the_target_chapter() {
        let directory = std::env::temp_dir().join(format!(
            "koharu-context-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        fs::create_dir_all(&directory).expect("create fixture");

        let two_pages = vec![vec![], vec![pair("前", "before")]];
        assert!(load_prior_context(&directory, 24).is_empty(), "a missing file");
        save_prior_context(&directory, 24, &two_pages).expect("save");
        assert!(directory.join("context-24.json").is_file());
        assert_eq!(load_prior_context(&directory, 24), two_pages);
        // 序号 23 读的是自己的文件，与 24 无关。
        assert!(load_prior_context(&directory, 23).is_empty());

        // 空就是没有上文，清掉旧文件而不是留一份陈旧基线。
        save_prior_context(&directory, 24, &[]).expect("clear");
        assert!(!directory.join("context-24.json").exists());

        // 上一版留下的扁平数组读不出来，按降级安静丢弃，下一批预扫描会重写。
        fs::write(directory.join("context-24.json"), b"[{\"source\":\"a\",\"translation\":\"b\"}]")
            .expect("write the old shape");
        assert!(
            load_prior_context(&directory, 24).is_empty(),
            "an unreadable shape degrades quietly"
        );
        assert!(!directory.join("context-24.json").exists(), "and is discarded");

        fs::write(directory.join("context-24.json"), b"{ not json").expect("write broken");
        assert!(load_prior_context(&directory, 24).is_empty(), "a broken file degrades quietly");
        assert!(!directory.join("context-24.json").exists(), "and is discarded");

        fs::remove_dir_all(&directory).expect("remove fixture");
    }
}
