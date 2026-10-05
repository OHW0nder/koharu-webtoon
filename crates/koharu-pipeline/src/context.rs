//! 章内上文：当前页之前若干页里已成对的原文与译文。
//!
//! **为什么在翻译阶段内部取，而不是从外面传进来。** 附加说明（`instructions`）是唯一能从外部换掉的
//! 通道，但它是整批共用的一段散文，而上文的意义恰恰在于逐页不同：50 页的章翻到第 25 页时，模型必须
//! 看到第 20 至 24 页的结果。从外部按页换 `instructions` 需要在两次 `execute` 之间重建阶段运行器，而
//! 一章只有一次 `execute`，所以那条路走不通。语境条目（`context`）是**已存在的公开字段**，语义上正好
//! 是「原文到译文的成对参考」，系统提示词里那句「不要回译这些条目」的约束随它一起出现（见
//! `docs/extending-koharu.md` §2.8 与 `koharu-translator` 的 `prompt.rs`）。填充点在翻译阶段内部，
//! 按 §3.2 属黄色改动。
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

/// 章内上文回溯页数的硬上限。
///
/// 上限保护的是上下文窗口：条漫一个切片页有十几个气泡，12 页就是上百条双语对照，加上正文与图片
/// 足以超出任何合理预算。回溯距离本身由用户按自己的 token 预算选（漫画级设置），但选到离谱的值
/// 时静默截断比直接炸掉请求好。
pub const MAX_CONTEXT_PAGES: u32 = 12;

/// 当前页之前 `pages` 页里已成对的原文与译文，按阅读顺序。
///
/// 取的是**最近**的若干页而不是最前面的：称谓与语气的一致性看的是最近的写法。
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
) -> anyhow::Result<Vec<TranslationContext>> {
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
    let start = index.saturating_sub(pages.min(MAX_CONTEXT_PAGES) as usize);
    let mut context = Vec::new();
    for preceding in &order[start..index] {
        collect_pairs(snapshot, *preceding, &mut context)?;
    }
    Ok(context)
}

/// 一个章节最后 `pages` 页里已成对的原文与译文，按阅读顺序。
///
/// 与 [`preceding_context`] 是同一概念的两个方向：那个锚定「当前页之前」，这个锚定「整章末尾」。
/// 章内上文用前者，跨章上文用后者——后者不参与翻译，只由漫画层在批量开始前派生并写进旁挂文件。
pub fn trailing_context(
    snapshot: &Snapshot,
    pages: u32,
) -> anyhow::Result<Vec<TranslationContext>> {
    let pages = pages.min(MAX_CONTEXT_PAGES);
    if pages == 0 {
        return Ok(Vec::new());
    }
    let order = snapshot
        .pages()
        .map(|candidate| candidate.id())
        .collect::<Vec<_>>();
    let start = order.len().saturating_sub(pages as usize);
    let mut context = Vec::new();
    for trailing in &order[start..] {
        collect_pairs(snapshot, *trailing, &mut context)?;
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
    /// `Some(false)` 是流水线生成的。
    async fn project(pages: &[Option<bool>]) -> (Snapshot, Vec<EntityId>) {
        let mut session = Session::memory().await.expect("memory session");
        let mut edit = session.snapshot().edit();
        let mut machine = Vec::new();
        let mut ids = Vec::new();
        for (index, translated) in pages.iter().enumerate() {
            let source = format!("src{index}");
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

    #[tokio::test]
    async fn the_window_is_the_pages_immediately_before_the_current_one() {
        let (snapshot, ids) = project(&[Some(true); 8]).await;

        // 这正是这个功能存在的理由：翻到第 5 页时看到第 1 至 4 页，而不是整章共用一份。
        let context = preceding_context(&snapshot, ids[4], 4).expect("context");
        assert_eq!(sources(&context), vec!["src0", "src1", "src2", "src3"]);

        // 窗口随后前移一页。
        let later = preceding_context(&snapshot, ids[5], 4).expect("context");
        assert_eq!(sources(&later), vec!["src1", "src2", "src3", "src4"]);

        // 第一页前面没有页，所以没有上文。
        assert!(
            preceding_context(&snapshot, ids[0], 4)
                .expect("context")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_page_without_a_translation_contributes_nothing() {
        // 前 4 页里第 2 页没翻，按「没翻过的就算了」处理：拿到 3 条，而不是继续往前凑满 4 条。
        let (snapshot, ids) = project(&[Some(true), None, Some(true), Some(true), Some(true)]).await;
        let context = preceding_context(&snapshot, ids[4], 4).expect("context");
        assert_eq!(sources(&context), vec!["src0", "src2", "src3"]);
    }

    #[tokio::test]
    async fn a_cold_start_only_gains_context_once_the_pages_exist() {
        // 全新章：前几页还没翻，上文为空。这正是「人工资料是冷启动机制」的含义。
        let (snapshot, ids) = project(&[None, None, None, None, Some(true)]).await;
        for index in 0..4 {
            assert!(
                preceding_context(&snapshot, ids[index], 4)
                    .expect("context")
                    .is_empty(),
                "page {index} has nothing before it"
            );
        }
        // 翻到第 5 页时，第 1 至 4 页依然没有译文。
        assert!(
            preceding_context(&snapshot, ids[4], 4)
                .expect("context")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn human_and_generated_translations_are_both_kept() {
        // 两种来源都收：人工编辑过的质量最高，但没有取舍它的理由。
        let (snapshot, ids) = project(&[Some(true), Some(false), Some(false), Some(true)]).await;
        let context = preceding_context(&snapshot, ids[3], 4).expect("context");
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
        let (snapshot, ids) = project(&[Some(true), Some(true)]).await;
        let missing = EntityId::new();
        assert!(
            preceding_context(&snapshot, missing, 4)
                .expect("context")
                .is_empty()
        );
        assert!(!ids.contains(&missing));
    }

    #[tokio::test]
    async fn zero_pages_disables_the_context() {
        let (snapshot, ids) = project(&[Some(true), Some(true)]).await;
        assert!(
            preceding_context(&snapshot, ids[1], 0)
                .expect("context")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_carrier_page_contributes_nothing_but_still_occupies_a_slot() {
        // 条漫导入会为承载完整原图的 carrier 页建一个场景页，它没有文本层。所以回溯 2 页时，
        // 如果前一页正好是 carrier，就只拿到 1 条对照——「没翻过的就算了」的自然结果。
        let (snapshot, ids) = project(&[Some(true), Some(true), Some(true)]).await;
        let context = preceding_context(&snapshot, ids[2], 2).expect("context");
        assert_eq!(sources(&context), vec!["src0", "src1"]);
    }

    #[tokio::test]
    async fn the_trailing_window_ends_at_the_last_page() {
        // 跨章上文要的是「整章末尾 N 页」，含最后一页，与锚定当前页的前一个函数不同。
        let (snapshot, _ids) = project(&[Some(true); 8]).await;
        let trailing = trailing_context(&snapshot, 4).expect("context");
        assert_eq!(sources(&trailing), vec!["src4", "src5", "src6", "src7"]);

        // 不足页数时取全部。
        let all = trailing_context(&snapshot, 99).expect("context");
        assert_eq!(all.len(), 8);
        assert_eq!(
            trailing_context(&snapshot, 0)
                .expect("context")
                .is_empty(),
            true
        );
    }

    #[tokio::test]
    async fn the_trailing_window_is_clamped_to_the_page_limit() {
        // 上限保护上下文窗口：填一个离谱的值时静默截断比直接炸掉请求好。
        let (snapshot, _ids) = project(&[Some(true); 20]).await;
        assert_eq!(
            trailing_context(&snapshot, u32::MAX)
                .expect("context")
                .len(),
            MAX_CONTEXT_PAGES as usize
        );
        assert_eq!(
            preceding_context(&snapshot, snapshot.pages().last().expect("a page").id(), u32::MAX)
                .expect("context")
                .len(),
            // 锚点页本身不计入，所以回溯 12 页就是 12 页的对照。
            MAX_CONTEXT_PAGES as usize
        );
    }
}
