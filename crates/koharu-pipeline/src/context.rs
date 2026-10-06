//! 上文窗口：翻译一页时，它之前最近若干页里已成对的原文与译文。
//!
//! **一个窗口，跨章连续。** 窗口以「当前页之前最近的 N 页」定义，N 是用户的 `context_pages`。翻到本章
//! 第 1 页时，窗口的 N 页全部来自上一章末尾；翻到第 2 页时，N−1 页来自上一章、1 页来自本章；本章的
//! 页数攒够了，上一章的部分就整体滑出窗口。窗口因此始终是 N 页，不会因为章内回溯与跨章回溯各算一遍
//! 而变成 2N 页，也不会让模型在整章里一直看着几十页前的写法。
//!
//! **为什么在翻译阶段内部取，而不是从外面传进来。** 附加说明（`instructions`）是唯一能从外部换掉的
//! 通道，但它是整批共用的一段散文，而上文的全部意义在于逐页不同：从外部按页换 `instructions` 需要在
//! 两次 `execute` 之间重建阶段运行器，而一章只有一次 `execute`，所以那条路走不通。语境条目（`context`）
//! 是**已存在的公开字段**，语义上正好是「原文到译文的成对参考」，系统提示词里那句「不要回译这些条目」
//! 的约束随它一起出现（见 `docs/extending-koharu.md` §2.8 与 `koharu-translator` 的 `prompt.rs`）。
//! 填充点在翻译阶段内部，按 §3.2 属黄色改动。
//!
//! **章的那一段从配置来。** 场景里只有当前章，上一章的页不在其中，所以窗口靠章边界的那部分由漫画层
//! 作为 `prior_chapter_context` 递进来：每章开始前写一次，翻完一章用刚跑出来的译文覆盖给下一章。
//!
//! **数据为什么已经在场。** `Execution` 每次提交后把场景快照换成新的，而每个新任务都拿最新的场景，
//! 所以翻第 N 页时第 N−1 页的译文已经提交。调度器用 `busy_stages` 保证同一时刻只有一个页在跑翻译，
//! 「上一页已提交」因此成立。**若将来改成章内跨页并发，这项必须先改为按运行序号维护队列**——那时
//! 「前 N 页」不再等价于「已翻好的前 N 页」。
//!
//! **回溯单位是切片后的页。** 一张条漫长图在导入时被切成多个 band，每个 band 是场景里的一页，所以
//! 「前 4 页」是四个切片页，大约覆盖原长图的三分之一。这与翻译的粒度一致：页是模型实际看到的单位。

use koharu_scene::{EntityId, Snapshot};
use koharu_translator::TranslationContext;

/// 上文窗口页数的硬上限。
///
/// 上限保护的是上下文窗口：条漫一个切片页有十几个气泡，12 页就是上百条双语对照，加上正文与图片
/// 足以超出任何合理预算。回溯距离本身由用户按自己的 token 预算选（漫画级设置），但选到离谱的值
/// 时静默截断比直接炸掉请求好。
pub const MAX_CONTEXT_PAGES: u32 = 12;

/// 当前页之前 `pages` 页里已成对的原文与译文，按阅读顺序。
///
/// 取的是**最近**的若干页而不是最前面的：称谓与语气的一致性看的是最近的写法。章内的部分就地取自
/// 场景，章外的部分取自 `prior`——它是上一章末尾那几页的同一批对照，按页分桶，翻译阶段只按窗口余量
/// 取它的尾部。两段拼起来正好是「当前页之前最近的 `pages` 页」。
///
/// 遇到没有译文的页就跳过，不会为了凑满条数继续往前找——回溯距离必须可预期。冷启动时前几页本来就没有
/// 上文，这正是「人工资料是冷启动机制」的含义。
///
/// 不按处理范围过滤：上文的语义是「这一页之前已经翻好的内容」，与「这次要处理哪些页」无关。单页重跑
/// 时前面的页不在范围内，但恰恰是那种场景最需要知道本章此前的写法。
pub fn preceding_context(
    snapshot: &Snapshot,
    page: EntityId,
    pages: u32,
    prior: &[Vec<TranslationContext>],
) -> anyhow::Result<Vec<TranslationContext>> {
    let pages = pages.min(MAX_CONTEXT_PAGES);
    if pages == 0 {
        return Ok(Vec::new());
    }
    let order = snapshot
        .pages()
        .map(|candidate| candidate.id())
        .collect::<Vec<_>>();
    let Some(index) = order.iter().position(|candidate| *candidate == page) else {
        // 页不在快照里意味着场景与请求不一致。宁可没有上文，也不要拿到错位的先例。
        return Ok(Vec::new());
    };
    let start = index.saturating_sub(pages as usize);
    // 窗口的页数先落在本章，再把上一章的尾部接在前面：本章占了 `index - start` 页，余下的名额从
    // `prior` 的末尾取，所以窗口恒为 `pages` 页，且随着本章推进把上一章的部分一页一页挤出去。
    let room = (pages as usize).saturating_sub(index - start);
    let mut context = Vec::new();
    for pairs in prior.iter().skip(prior.len().saturating_sub(room)) {
        context.extend(pairs.iter().cloned());
    }
    for preceding in &order[start..index] {
        collect_pairs(snapshot, *preceding, &mut context)?;
    }
    Ok(context)
}

/// 一个章节末尾 `pages` 页里已成对的原文与译文，每页一个桶，按阅读顺序。
///
/// 这是 [`preceding_context`] 的窗口在章边界另一侧的输入，与它共用 [`collect_pairs`]：判断规则必须
/// 一致，否则同一条对照在章内与跨章两个来源里会得到不同的取舍。
///
/// 桶保留没有译文的页（内层是空数组）：窗口以页计，所以一页没翻过就占一个名额而不产出对照。丢掉空桶
/// 会让回溯距离在章首悄悄变长，而那正是用户设的那个数字所约束的东西。
pub fn trailing_pages(
    snapshot: &Snapshot,
    pages: u32,
) -> anyhow::Result<Vec<Vec<TranslationContext>>> {
    let pages = pages.min(MAX_CONTEXT_PAGES);
    if pages == 0 {
        return Ok(Vec::new());
    }
    let order = snapshot
        .pages()
        .map(|candidate| candidate.id())
        .collect::<Vec<_>>();
    let start = order.len().saturating_sub(pages as usize);
    let mut context = Vec::with_capacity(order.len() - start);
    for trailing in &order[start..] {
        let mut pairs = Vec::new();
        collect_pairs(snapshot, *trailing, &mut pairs)?;
        context.push(pairs);
    }
    Ok(context)
}

/// 一页里所有成对的原文与译文，按该页的阅读顺序。
fn collect_pairs(
    snapshot: &Snapshot,
    page: EntityId,
    into: &mut Vec<TranslationContext>,
) -> anyhow::Result<()> {
    let Some(group) = snapshot.page(page)?.text_group()? else {
        return Ok(());
    };
    for layer in group.text_layers()? {
        let content = layer.content()?;
        let Some(source) = content.source()? else {
            continue;
        };
        let Some(translation) = content.translation()? else {
            continue;
        };
        if source.text.value.trim().is_empty() || translation.text.value.trim().is_empty() {
            continue;
        }
        into.push(TranslationContext {
            source: source.text.value,
            translation: translation.text.value,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use koharu_scene::{
        At, Authored, Generation, Origin, PageDraft, ProducerId, Session, SourceText, TextContent,
        TextLayout, TextLayoutKind, Translation,
    };

    use super::*;

    /// 建一个项目，`pages` 的第 i 项描述第 i 页：`None` 没有译文，`Some(true)` 是人工写的，
    /// `Some(false)` 是流水线生成的。原文是 `{prefix}{index}`，前缀用来区分不同章节的同名页。
    async fn project(prefix: &str, pages: &[Option<bool>]) -> (Snapshot, Vec<EntityId>) {
        let mut session = Session::memory().await.expect("memory session");
        let mut edit = session.snapshot().edit();
        let mut machine = Vec::new();
        let mut ids = Vec::new();
        for (index, translated) in pages.iter().enumerate() {
            let source = format!("{prefix}{index}");
            let page = edit
                .add_page(PageDraft::new(source.clone(), 100.0, 100.0), At::End)
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
                    text: Authored::user(source),
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
            ids.push(page);
            match translated {
                None => {}
                Some(true) => edit
                    .set(
                        content,
                        &Translation {
                            text: Authored::user(format!("tr{index}")),
                            language: None,
                        },
                    )
                    .expect("set translation"),
                Some(false) => machine.push((content, index)),
            }
        }
        let snapshot = session
            .commit(edit.finish().expect("finish"))
            .await
            .expect("commit")
            .snapshot;
        // 流水线写的译文必须走 `edit_as`：`Edit::set` 会把组件来源统一盖成 `User`，所以直接
        // `set` 一个 `Origin::Generated` 是写不进去的。
        let mut generated = snapshot.edit_as(generation());
        for (content, index) in machine {
            generated
                .set(
                    content,
                    &Translation {
                        text: Authored::generated(format!("tr{index}"), generation()),
                        language: None,
                    },
                )
                .expect("set generated translation");
        }
        let snapshot = session
            .commit(generated.finish().expect("finish"))
            .await
            .expect("commit")
            .snapshot;
        (snapshot, ids)
    }

    fn generation() -> Generation {
        Generation::new(ProducerId::new("dev.koharu.pipeline.translation").expect("producer"))
    }

    fn sources(context: &[TranslationContext]) -> Vec<&str> {
        context.iter().map(|entry| entry.source.as_str()).collect()
    }

    /// 按页摊平嵌套的上文窗口，让断言能一次看清「哪一页贡献了什么」。
    fn paged(pages: &[Vec<TranslationContext>]) -> Vec<Vec<&str>> {
        pages
            .iter()
            .map(|pairs| pairs.iter().map(|entry| entry.source.as_str()).collect())
            .collect()
    }

    #[tokio::test]
    async fn the_window_is_the_pages_immediately_before_the_current_one() {
        let (snapshot, ids) = project("src", &[Some(true); 8]).await;

        // 这正是这个功能存在的理由：翻到第 5 页时看到第 1 至 4 页，而不是整章共用一份。
        let context = preceding_context(&snapshot, ids[4], 4, &[]).expect("context");
        assert_eq!(sources(&context), vec!["src0", "src1", "src2", "src3"]);

        // 窗口随后前移一页。
        let later = preceding_context(&snapshot, ids[5], 4, &[]).expect("context");
        assert_eq!(sources(&later), vec!["src1", "src2", "src3", "src4"]);

        // 第一页前面没有页，所以没有上文。
        assert!(
            preceding_context(&snapshot, ids[0], 4, &[])
                .expect("context")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_page_without_a_translation_contributes_nothing() {
        // 前 4 页里第 2 页没翻，按「没翻过的就算了」处理：拿到 3 条，而不是继续往前凑满 4 条。
        let (snapshot, ids) = project("src", &[Some(true), None, Some(true), Some(true), Some(true)]).await;
        let context = preceding_context(&snapshot, ids[4], 4, &[]).expect("context");
        assert_eq!(sources(&context), vec!["src0", "src2", "src3"]);
    }

    #[tokio::test]
    async fn a_cold_start_only_gains_context_once_the_pages_exist() {
        // 全新章：前几页还没翻，上文为空。这正是「人工资料是冷启动机制」的含义。
        let (snapshot, ids) = project("src", &[None, None, None, None, Some(true)]).await;
        for index in 0..4 {
            assert!(
                preceding_context(&snapshot, ids[index], 4, &[])
                    .expect("context")
                    .is_empty(),
                "page {index} has nothing before it"
            );
        }
        // 翻到第 5 页时，第 1 至 4 页依然没有译文。
        assert!(
            preceding_context(&snapshot, ids[4], 4, &[])
                .expect("context")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn human_and_generated_translations_are_both_kept() {
        // 两种来源都收：人工编辑过的质量最高，但没有取舍它的理由。
        let (snapshot, ids) = project("src", &[Some(true), Some(false), Some(false), Some(true)]).await;
        let context = preceding_context(&snapshot, ids[3], 4, &[]).expect("context");
        assert_eq!(sources(&context), vec!["src0", "src1", "src2"]);
        assert_eq!(
            context
                .iter()
                .map(|entry| entry.translation.as_str())
                .collect::<Vec<_>>(),
            vec!["tr0", "tr1", "tr2"]
        );
    }

    #[tokio::test]
    async fn a_page_outside_the_snapshot_has_no_context() {
        let (snapshot, ids) = project("src", &[Some(true), Some(true)]).await;
        let missing = EntityId::new();
        assert!(
            preceding_context(&snapshot, missing, 4, &[])
                .expect("context")
                .is_empty()
        );
        assert!(!ids.contains(&missing));
    }

    #[tokio::test]
    async fn zero_pages_disables_the_context() {
        let (snapshot, ids) = project("src", &[Some(true), Some(true)]).await;
        // 上一章的种子不为空，但 0 就是不注入：用户的旋钮优先于递进来的数据。
        let prior = vec![vec![], vec![]];
        assert!(
            preceding_context(&snapshot, ids[1], 0, &prior)
                .expect("context")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_carrier_page_contributes_nothing_but_still_occupies_a_slot() {
        // 条漫导入会为承载完整原图的 carrier 页建一个场景页，它没有文本层。所以回溯 2 页时，
        // 如果前一页正好是 carrier，就只拿到 1 条对照——「没翻过的就算了」的自然结果。
        let (snapshot, ids) = project("src", &[Some(true), Some(true), Some(true)]).await;
        let context = preceding_context(&snapshot, ids[2], 2, &[]).expect("context");
        assert_eq!(sources(&context), vec!["src0", "src1"]);
    }

    #[tokio::test]
    async fn the_window_slides_across_the_chapter_boundary() {
        // 上一章 8 页，窗口 4 页。这是本次改动的全部要点：窗口跨章连续，总量恒为 4 页。
        let (previous, _) = project("p", &[Some(true); 8]).await;
        let prior = trailing_pages(&previous, 4).expect("prior");
        let (current, ids) = project("c", &[Some(true); 3]).await;
        let window = |index: usize| {
            let context = preceding_context(&current, ids[index], 4, &prior).expect("context");
            context.iter().map(|entry| entry.source.clone()).collect::<Vec<_>>()
        };

        // 本章第 1 页：4 页名额全在上一章末尾。
        assert_eq!(window(0), vec!["p4", "p5", "p6", "p7"]);
        // 本章第 2 页：上一章让出最早的一页，换成本章第 1 页。
        assert_eq!(window(1), vec!["p5", "p6", "p7", "c0"]);
        // 本章第 3 页：再让出一页。
        assert_eq!(window(2), vec!["p6", "p7", "c0", "c1"]);

        // 本章够长时上一章整体滑出，窗口回到纯章内——不是章内 4 页再加上一章 4 页。
        let (long, _) = project("c", &[Some(true); 6]).await;
        let later = preceding_context(
            &long,
            long.pages().last().expect("a page").id(),
            4,
            &prior,
        )
        .expect("context");
        assert_eq!(sources(&later), vec!["c1", "c2", "c3", "c4"]);
    }

    #[tokio::test]
    async fn an_untranslated_prior_page_still_consumes_a_window_slot() {
        // 上一章倒数第二页没翻：它仍然占一个名额，只是不产出对照。与章内的取舍是同一条规则，
        // 否则章首的回溯距离会悄悄变长。
        let (previous, _) = project("p", &[Some(true), None, Some(true)]).await;
        let prior = trailing_pages(&previous, 2).expect("prior");
        assert_eq!(paged(&prior), vec![vec![], vec!["p2"]]);

        let (current, ids) = project("c", &[None]).await;
        let context = preceding_context(&current, ids[0], 2, &prior).expect("context");
        assert_eq!(sources(&context), vec!["p2"], "窗口是 2 页，对照只有 1 条");
    }

    #[tokio::test]
    async fn the_trailing_window_ends_at_the_last_page() {
        // 跨章那部分要的是「整章末尾 N 页」，含最后一页，与锚定当前页的前一个函数不同。
        let (snapshot, _ids) = project("src", &[Some(true); 8]).await;
        let trailing = trailing_pages(&snapshot, 4).expect("pages");
        assert_eq!(
            paged(&trailing),
            vec![vec!["src4"], vec!["src5"], vec!["src6"], vec!["src7"]]
        );

        // 不足页数时取全部。
        assert_eq!(trailing_pages(&snapshot, 99).expect("pages").len(), 8);
        assert!(trailing_pages(&snapshot, 0).expect("pages").is_empty());
    }

    #[tokio::test]
    async fn the_trailing_window_is_clamped_to_the_page_limit() {
        // 上限保护上下文窗口：填一个离谱的值时静默截断比直接炸掉请求好。
        let (snapshot, _ids) = project("src", &[Some(true); 20]).await;
        assert_eq!(
            trailing_pages(&snapshot, u32::MAX)
                .expect("pages")
                .len(),
            MAX_CONTEXT_PAGES as usize
        );
        assert_eq!(
            preceding_context(
                &snapshot,
                snapshot.pages().last().expect("a page").id(),
                u32::MAX,
                &[]
            )
                .expect("context")
                .len(),
            // 锚点页本身不计入，所以回溯 12 页就是 12 页的对照。
            MAX_CONTEXT_PAGES as usize
        );
    }
}
