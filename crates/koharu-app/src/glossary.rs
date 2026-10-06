//! 术语表的读写、命中过滤与渲染。
//!
//! 术语表挂在漫画目录里，与 `series.json` 同级，不进内核的场景。这一模块只做三件事：把
//! `glossary.json` 读进内存、按待译内容筛出命中的条目、把命中的条目渲染成一段可以追加到翻译
//! 附加说明里的散文。
//!
//! 边界在这里划是有理由的：**术语表的行为正确性全部在匹配规则里**，而匹配规则是整条链上唯一一处
//! 会静默产出错误译名的地方——它不报错，只是把「正确」换成「看起来还行」。因此这一层全部写成纯
//! 函数加一对文件操作，可以脱离场景、流水线与翻译层单测。词条从哪来（导入与抽取）、什么时候注入
//! （`injection`）、预算怎么分（上下文管线）都另有归属。
//!
//! 匹配与渲染的口径来自 `docs/reference/koharu-glossary-design.md` §4.1 与 §3.1：全文匹配，不做
//! 模糊匹配；多词术语任一词元命中即算命中，因为简称是正文里的常态；拉丁与西里尔要求词边界，
//! 中日韩关闭词边界按子串匹配。

use std::{collections::BTreeMap, fs, io, path::Path};

use anyhow::{Context as _, Result};

use crate::commands::series::{
    GLOSSARY_FILE, Glossary, GlossaryEntry, GlossaryKind, normalize_glossary_source,
    validate_glossary,
};

/// 渲染出的清单抬头。
const HEADER: &str = "Terminology — these source terms have fixed target translations. Use them exactly as given. Do not translate, output, or comment on this list.";

/// 渲染出的清单收尾。
const FOOTER: &str = "These are reference only. Apply them to the matching source text.";

/// 读一部漫画的术语表。文件不存在是合法状态，返回空表。
///
/// 空表的 `enabled` 为 `false`，所以还没建过术语表的漫画天然不注入，调用方不需要额外判空。
pub(crate) fn load(dir: &Path) -> Result<Glossary> {
    let path = dir.join(GLOSSARY_FILE);
    let contents = match fs::read_to_string(&path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Glossary::default()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let glossary: Glossary = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse the glossary at {}", path.display()))?;
    // 自己写出去的文件一定过得了校验，这里失败只可能是手工编辑或外部写入。与其带着重复原文继续
    // 注入，不如报错：重复的原文会让同一条清单里出现两个互相矛盾的译名，比不注入更难排查。
    validate_glossary(&glossary)
        .with_context(|| format!("the glossary at {} is not valid", path.display()))?;
    Ok(glossary)
}

/// 写回术语表。先校验，再写临时文件改名，中途失败不会留下半份表。
pub(crate) fn save(dir: &Path, glossary: &Glossary) -> Result<()> {
    validate_glossary(glossary).context("refusing to write an invalid glossary")?;
    fs::create_dir_all(dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let contents =
        serde_json::to_string_pretty(glossary).context("failed to encode the glossary")?;
    let path = dir.join(GLOSSARY_FILE);
    let temporary = dir.join(format!("{GLOSSARY_FILE}.tmp"));
    fs::write(&temporary, contents)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, &path)
        .with_context(|| format!("failed to publish {}", path.display()))?;
    Ok(())
}

/// 待译内容里命中的条目，按「类别 → 归一化原文」排序。
///
/// 顺序必须确定：同一份输入要产出逐字节相同的提示词，否则同一页重跑会得到不同的翻译，而缓存键
/// 又是按提示词算的。
///
/// `haystack` 是当前章的原文而不是单页原文，因此这里的判断是一章一次全量扫描而不是逐页扫描
/// （`docs/reference/koharu-glossary-design.md` §4.3）。归一化只做一次：放进循环里就是每条术语
/// 重扫一遍全文，术语表上百条时会在批量预扫描上放大成明显的耗时。
pub(crate) fn matched_entries<'a>(
    glossary: &'a Glossary,
    haystack: &str,
) -> Vec<&'a GlossaryEntry> {
    if !glossary.enabled {
        return Vec::new();
    }
    let normalized = normalize_haystack(haystack);
    // 用 BTreeMap 收集而不是 Vec 排序：去重与排序一次完成，结果与词条的写入顺序无关。
    // 键是「类别 + 归一化原文」，与 `Glossary::duplicate_keys` 同口径，因此同一键在结果里必然只
    // 剩一条，键内按 id 取最小的那条——这同时就是要求的 id 序。
    let mut hits: BTreeMap<(GlossaryKind, String), &GlossaryEntry> = BTreeMap::new();
    for entry in &glossary.entries {
        if !entry.is_injectable() {
            continue;
        }
        let source = normalize_glossary_source(&entry.source);
        if !occurs(&source, &normalized) {
            continue;
        }
        hits.entry((entry.kind, source))
            .and_modify(|kept| {
                if entry.id < kept.id {
                    *kept = entry;
                }
            })
            .or_insert(entry);
    }
    hits.into_values().collect()
}

/// 把命中的条目渲染成一段可以直接追加到附加说明里的散文。
///
/// 抬头与收尾那两句约束不是润色。系统提示词里那段「不要回译这些条目」是随语境条目字段一起出现的，
/// 走附加说明通道时它不会出现（`docs/reference/koharu-glossary-design.md` §3.1）。漏掉它，模型会把
/// 清单里的译名当成待处理内容，原样抄进译文——这是散文通道最容易漏的一步。
pub(crate) fn render(glossary: &Glossary, matched: &[&GlossaryEntry]) -> String {
    if !glossary.enabled {
        return String::new();
    }
    let mut lines = Vec::with_capacity(matched.len() + 2);
    lines.push(HEADER.to_owned());
    for entry in matched {
        let Some(translation) = entry
            .translation
            .as_deref()
            .map(one_line)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let source = one_line(&entry.source);
        if source.is_empty() {
            continue;
        }
        let mut line = format!("- {source} → {translation}");
        let note = one_line(&entry.note);
        if !note.is_empty() {
            line.push_str(&format!("（{note}）"));
        }
        lines.push(line);
    }
    // 命中的条目全被跳过时不能只留一个孤零零的抬头，那会让模型收到一句没有内容的指令。
    if lines.len() == 1 {
        return String::new();
    }
    lines.push(FOOTER.to_owned());
    lines.join("\n")
}

/// 一段归一化原文是否出现在归一化全文里。
///
/// **多词术语任一词元命中即算命中。** 人名与专有名词在正文里常以简称出现：作者写全名建
/// 立一次，之后只叫名（`Jack Hansen` 的 `Jack`）。要求整串会让那些页拿不到定稿译名，而译名
/// 在页与页之间不一致是读者一眼能看出的缺陷，因此宁可多注入一条词条。
///
/// 词元各自按词边界判定，所以 `Jack` 仍然不会命中 `Jackhammer`。
fn occurs(source: &str, haystack: &str) -> bool {
    source.split_whitespace().any(|token| occurs_token(token, haystack))
}

fn occurs_token(token: &str, haystack: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let boundary = needs_word_boundary(token);
    let mut from = 0;
    while let Some(offset) = haystack[from..].find(token) {
        let start = from + offset;
        let end = start + token.len();
        if !boundary || bounded_at(token, haystack, start, end) {
            return true;
        }
        // 从命中的下一个字符接着找，而不是从命中末尾。原文含标点时（`a-a`）两个命中位置可以重叠，
        // 跳过重叠段会漏掉后面那个边界合规的命中。步长取首字符的字节长度，保证切片落在字符边界上。
        from = start + token.chars().next().map_or(1, char::len_utf8);
    }
    false
}

/// 一次命中是否满足词边界。
///
/// 只在原文的首尾字符参与词边界时才检查那一侧。原文写成 `Ann.` 时右边界天然由那个点满足，再要求
/// 命中处后面是非字母数字反而会漏掉 `Ann.x` 这种写法。
fn bounded_at(source: &str, haystack: &str, start: usize, end: usize) -> bool {
    let blocked_left = source.chars().next().is_some_and(is_word_character)
        && haystack[..start].chars().next_back().is_some_and(is_word_character);
    let blocked_right = source.chars().next_back().is_some_and(is_word_character)
        && haystack[end..].chars().next().is_some_and(is_word_character);
    !blocked_left && !blocked_right
}

/// 这段原文是否要求词边界。
///
/// 口径：原文的每个字符都属于「拉丁、西里尔、数字、西文标点与空白」时才要求。出现一个不属于这一类
/// 的字符就整条退化成子串匹配——中日韩文字不用空格分词，对它们要求边界会让 `ワープ` 在 `ワープは`
/// 里判成不命中。原文里混了拉丁与假名时同样走这条，因为含假名那一侧本来就没有边界可言。
fn needs_word_boundary(source: &str) -> bool {
    source
        .chars()
        .all(|character| is_word_character(character) || !character.is_alphanumeric())
}

/// 一个字符是否参与词边界判定。
///
/// 判据是「这个文字用不用空格分词」，而不是 `is_alphabetic`：后者对汉字与假名同样为真，用它会把
/// `アリスは` 里的 `アリス` 判成不命中。ASCII 标点与空白不参与，它们正是边界本身。
fn is_word_character(character: char) -> bool {
    if character.is_ascii() {
        return character.is_ascii_alphanumeric();
    }
    (character.is_alphabetic() || character.is_numeric()) && is_spaced_alphabet(character)
}

/// 用空格分词的文字：拉丁、西里尔，以及同样用空格分词的希腊文。
///
/// 范围写死不查 `Script` 属性：本仓库没有 Unicode 属性表依赖，而这个清单在术语表这个场景下是
/// 封闭的——作品原文只会是拉丁、西里尔与中日韩。没有落进这些范围的字母（阿拉伯文、希伯来文、天城文
/// 等同样不用空格分词）自动退化成子串匹配，方向是安全的。
fn is_spaced_alphabet(character: char) -> bool {
    matches!(
        character as u32,
        // 拉丁补充、拉丁扩展 A/B
        0x00C0..=0x024F
        // 希腊文
        | 0x0370..=0x03FF
        // 西里尔文
        | 0x0400..=0x052F
        // 拉丁补充附加
        | 0x1E00..=0x1EFF
        // 拉丁扩展 C
        | 0x2C60..=0x2C7F
        // 西里尔扩展 A/B
        | 0xA640..=0xA69F
    )
}

/// 归一化待译全文：NFKC 之后转小写。
///
/// 与 `normalize_glossary_source` 的唯一区别是**不折叠空白**。折叠会把气泡之间的换行变成一个空格，
/// 于是上一格末尾的 `Ann` 与下一格开头的 `Smith` 会被拼成 `ann smith` 命中一条双词术语。那是跨气泡的
/// 假命中，比漏命中坏得多，所以宁可漏。
fn normalize_haystack(haystack: &str) -> String {
    icu_normalizer::ComposingNormalizerBorrowed::new_nfkc()
        .normalize_iter(haystack.chars())
        .collect::<String>()
        .to_lowercase()
}

/// 把字段里的空白折叠成单空格。
///
/// 术语里混进换行会把一条清单拆成两行，模型读到半条术语比读不到更糟。
fn one_line(value: &str) -> String {
    let mut line = String::with_capacity(value.len());
    for word in value.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    line
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::commands::series::{GlossaryEntryId, GlossaryValueOrigin};

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

    fn typed(kind: GlossaryKind, source: &str, translation: &str) -> GlossaryEntry {
        GlossaryEntry {
            kind,
            ..entry(source, translation)
        }
    }

    fn glossary(entries: Vec<GlossaryEntry>) -> Glossary {
        Glossary {
            enabled: true,
            entries,
            ..Glossary::default()
        }
    }

    fn sources(matched: &[&GlossaryEntry]) -> Vec<String> {
        matched
            .iter()
            .map(|entry| entry.source.clone())
            .collect()
    }

    fn fixture(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("koharu-glossary-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create fixture directory");
        path
    }

    #[test]
    fn matching_ignores_width_case_and_extra_whitespace() {
        // 漫画文本里全角与半角拉丁混用，不做 NFKC 就会把同一个词当成两个。
        //
        // 注意 `Ａ ＬＩＣＥ` 那种「全角字母之间带空格」的写法归一化后是 `a l i c e`，它与
        // `alice` 本来就是两个不同的字符串，不命中是对的。折叠空白只处理排版产生的多余空白，不
        // 负责把逐字加空格还原成紧凑写法——那属于模糊匹配，而这一层刻意不做模糊匹配。
        let table = glossary(vec![
            entry("ＡＬＩＣＥ", "Alice"),
            entry("  Warp   Drive  ", "Warp Drive"),
        ]);
        let matched = matched_entries(&table, "alice の warp drive");
        assert_eq!(sources(&matched), vec!["ＡＬＩＣＥ", "  Warp   Drive  "]);
    }

    #[test]
    fn a_repeated_source_keeps_the_entry_with_the_smallest_id() {
        let first = entry("Alice", "Alice");
        let second = entry("alice", "a duplicate");
        let (kept, dropped) = if first.id < second.id {
            (first.id, second.id)
        } else {
            (second.id, first.id)
        };
        // 写入顺序与 id 相反：去留必须由 id 决定，而不是谁写在前面。
        let table = glossary(vec![second, first]);
        let matched = matched_entries(&table, "alice");
        assert_eq!(matched.len(), 1);
        assert_eq!(matched[0].id, kept);
        assert_ne!(matched[0].id, dropped);
    }

    #[test]
    fn the_same_source_under_two_kinds_is_not_a_duplicate() {
        // 同一段原文挂在两个类别下是有意义的资料（人名与地名同名），不能被去重吃掉。
        let table = glossary(vec![
            typed(GlossaryKind::Item, "アリス", "Alice's pendant"),
            typed(GlossaryKind::Person, "アリス", "Alice"),
        ]);
        let matched = matched_entries(&table, "アリスが");
        assert_eq!(matched.len(), 2);
        assert_eq!(matched[0].kind, GlossaryKind::Person, "类别序在前");
        assert_eq!(matched[1].kind, GlossaryKind::Item);
    }

    #[test]
    fn a_latin_term_needs_word_boundaries() {
        let table = glossary(vec![entry("Ann", "Ann")]);
        assert!(
            matched_entries(&table, "Anna waved").is_empty(),
            "Ann must not reach into Anna"
        );
        assert_eq!(
            matched_entries(&table, "Ann. waved").len(),
            1,
            "a period ends the word"
        );
        assert_eq!(
            matched_entries(&table, "(Ann) waved").len(),
            1,
            "brackets end the word"
        );
        assert_eq!(
            matched_entries(&table, "waved, Ann").len(),
            1,
            "a comma starts the word and the end of the text closes it"
        );
    }

    #[test]
    fn a_multi_word_term_matches_on_any_of_its_words() {
        // 简称是正文里的常态：人名只叫名，机构只叫后半截。要求整串会让那些页拿不到定稿译名，
        // 于是同一角色在有的页叫「贾科汉森」、有的页叫「杰克」。
        let table = glossary(vec![entry("Jack Hansen", "贾科汉森")]);
        for haystack in [
            "jack hansen は", // 整串
            "hey jack",       // 只叫名
            "hansen said",    // 只叫姓
            "the JACK HANSEN", // 大小写
        ] {
            assert_eq!(
                matched_entries(&table, haystack).len(),
                1,
                "should match: {haystack}"
            );
        }
        for haystack in [
            "jackhammer",   // 词边界挡住了
            "hansenite",    // 同上
            "no name here", // 一个词元都没出现
        ] {
            assert!(
                matched_entries(&table, haystack).is_empty(),
                "should not match: {haystack}"
            );
        }
    }

    #[test]
    fn a_cjk_term_matches_inside_a_sentence() {
        // 假名不用空格分词，对它要求边界会让最常见的写法判成不命中。
        let table = glossary(vec![entry("アリス", "Alice")]);
        assert_eq!(matched_entries(&table, "アリスは笑った").len(), 1);
        assert_eq!(matched_entries(&table, "……アリス……").len(), 1);
    }

    #[test]
    fn a_latin_term_does_not_reach_into_a_longer_word_in_japanese_text() {
        let table = glossary(vec![entry("Privat", "Prived")]);
        assert!(
            matched_entries(&table, "これは Private な話").is_empty(),
            "the surrounding text being Japanese does not switch off the boundary rule"
        );
        assert_eq!(
            matched_entries(&table, "これは Privat な話").len(),
            1,
            "a standalone Latin term still matches inside Japanese text"
        );
    }

    #[test]
    fn the_result_does_not_depend_on_the_order_of_the_entries() {
        let entries = vec![
            typed(GlossaryKind::Person, "アリス", "Alice"),
            typed(GlossaryKind::Place, "Warp", "Warp"),
            typed(GlossaryKind::Item, "刀", "剑"),
            typed(GlossaryKind::Term, "Ann", "Ann"),
        ];
        let mut shuffled = entries.clone();
        let straight = sources(&matched_entries(&glossary(entries), "アリス、Ann、Warp、刀"));
        shuffled.reverse();
        let reversed = sources(&matched_entries(&glossary(shuffled), "アリス、Ann、Warp、刀"));
        assert_eq!(straight, reversed);
        // 类别序 → 归一化原文序，与声明顺序无关的那一层也要定死。
        assert_eq!(straight, vec!["アリス", "Warp", "刀", "Ann"]);
    }

    #[test]
    fn a_disabled_glossary_injects_nothing() {
        let mut table = glossary(vec![entry("アリス", "Alice")]);
        assert_eq!(matched_entries(&table, "アリス").len(), 1);
        assert!(!render(&table, &matched_entries(&table, "アリス")).is_empty());

        table.enabled = false;
        assert!(matched_entries(&table, "アリス").is_empty());
        assert_eq!(render(&table, &[]), "");
    }

    #[test]
    fn an_untranslated_or_disabled_entry_is_never_matched() {
        let mut untranslated = entry("アリス", "Alice");
        untranslated.translation = None;
        let mut blank = entry("刀", "   ");
        blank.translation = Some(" ".to_owned());
        let mut disabled = entry("Warp", "Warp");
        disabled.enabled = false;

        let table = glossary(vec![untranslated, blank, disabled]);
        assert!(matched_entries(&table, "アリス 刀 Warp").is_empty());
    }

    #[test]
    fn rendering_nothing_yields_nothing() {
        let table = glossary(vec![entry("アリス", "Alice")]);
        assert_eq!(render(&table, &[]), "");
    }

    #[test]
    fn the_rendered_list_carries_its_own_do_not_translate_constraint() {
        let table = glossary(vec![entry("アリス", "Alice"), entry("ワープ", "Warp")]);
        let matched = matched_entries(&table, "アリスとワープ");
        let text = render(&table, &matched);
        assert_eq!(
            text,
            "Terminology — these source terms have fixed target translations. \
Use them exactly as given. Do not translate, output, or comment on this list.\n\
- アリス → Alice\n\
- ワープ → Warp\n\
These are reference only. Apply them to the matching source text."
        );
    }

    #[test]
    fn a_note_travels_with_its_term_in_parentheses() {
        let mut blade = entry("刀", "剑");
        blade.note = "只在战斗场景指武器".to_owned();
        let table = glossary(vec![blade, entry("Alice", "Alice")]);
        let matched = matched_entries(&table, "刀 と Alice");
        let text = render(&table, &matched);
        assert!(text.contains("- 刀 → 剑（只在战斗场景指武器）"), "{text}");
        assert!(text.contains("- Alice → Alice"), "{text}");
        assert!(!text.contains("()"), "{text}");
    }

    #[test]
    fn a_saved_glossary_reads_back_identical() {
        let directory = fixture("round-trip");
        let mut table = glossary(vec![typed(GlossaryKind::Person, "アリス", "Alice")]);
        table.source_language = Some("ja".to_owned());
        table.target_language = Some("en".to_owned());
        table.source_fingerprint = Some("fingerprint".to_owned());
        table.entries[0].note = "只在战斗场景指武器".to_owned();
        table.entries[0].examples = vec!["アリスは笑った".to_owned()];
        table.entries[0].confidence = Some(0.75);
        table.entries[0].occurrence_count = 12;
        table.entries[0].present_in_last_scan = false;

        save(&directory, &table).expect("save the glossary");
        let loaded = load(&directory).expect("load the glossary");

        assert_eq!(loaded.enabled, table.enabled);
        assert_eq!(loaded.source_language, table.source_language);
        assert_eq!(loaded.target_language, table.target_language);
        assert_eq!(loaded.source_fingerprint, table.source_fingerprint);
        assert_eq!(loaded.entries.len(), 1);
        assert_eq!(loaded.entries[0].id, table.entries[0].id);
        assert_eq!(loaded.entries[0].source, "アリス");
        assert_eq!(loaded.entries[0].translation.as_deref(), Some("Alice"));
        assert_eq!(loaded.entries[0].kind, GlossaryKind::Person);
        assert_eq!(loaded.entries[0].note, "只在战斗场景指武器");
        assert_eq!(loaded.entries[0].examples, vec!["アリスは笑った"]);
        assert_eq!(loaded.entries[0].confidence, Some(0.75));
        assert_eq!(loaded.entries[0].occurrence_count, 12);
        assert!(!loaded.entries[0].present_in_last_scan);

        fs::remove_dir_all(&directory).expect("remove the fixture");
    }

    #[test]
    fn a_missing_glossary_is_an_empty_disabled_table() {
        let directory = fixture("missing");
        let loaded = load(&directory).expect("a missing glossary is not an error");
        assert!(!loaded.enabled, "a series without a glossary injects nothing");
        assert!(loaded.entries.is_empty());

        fs::write(directory.join(GLOSSARY_FILE), b"{ not json")
            .expect("write a broken glossary");
        assert!(load(&directory).is_err(), "a broken file must not read as empty");

        fs::remove_dir_all(&directory).expect("remove the fixture");
    }

    #[test]
    fn an_invalid_glossary_never_reaches_the_disk() {
        let directory = fixture("invalid");
        // 归一化之后重复的「原文 + 类别」。
        let table = glossary(vec![entry("Alice", "Alice"), entry("alice", "a duplicate")]);
        assert!(save(&directory, &table).is_err());
        assert!(!directory.join(GLOSSARY_FILE).exists());

        let mut blank = glossary(vec![entry("Alice", "Alice")]);
        blank.entries[0].source = "   ".to_owned();
        assert!(save(&directory, &blank).is_err());
        assert!(!directory.join(GLOSSARY_FILE).exists());

        fs::remove_dir_all(&directory).expect("remove the fixture");
    }
}
