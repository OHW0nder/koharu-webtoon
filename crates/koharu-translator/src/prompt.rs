use std::fmt::Write;

use anyhow::Context;
use indoc::indoc;
use serde::{Deserialize, Deserializer, Serialize, de};
use serde_json::{Value, json};

use crate::{Language, TranslationContext, TranslationRequest};

pub(crate) fn prompts(request: &TranslationRequest) -> anyhow::Result<(String, String)> {
    let input = TranslationInput {
        source_language: request.source_language,
        target_language: request.target_language,
        context: &request.context,
        segments: request
            .segments
            .iter()
            .enumerate()
            .map(|(id, text)| TranslationInputSegment { id, text })
            .collect(),
    };
    let user = serde_json::to_string(&input).context("failed to serialize translation input")?;
    Ok((translation_system_prompt(request), user))
}

pub(crate) fn translations(
    provider: &str,
    text: &str,
    source_segments: &[String],
) -> anyhow::Result<Vec<String>> {
    // A response that does not close its object is handed over whole so the
    // error names the real failure (a truncated value, not a missing one).
    let object = json_object(text).unwrap_or(text);
    let output = serde_json::from_str::<TranslationOutput>(object).with_context(|| {
        format!(
            "{provider} returned invalid translation JSON for {} segments; response was: {}",
            source_segments.len(),
            snippet(text),
        )
    })?;
    let mut translations = source_segments.to_vec();
    let mut translated = vec![false; source_segments.len()];

    for translation in output.translations {
        if translation.id < translations.len() && !translated[translation.id] {
            translations[translation.id] = translation.text;
            translated[translation.id] = true;
        }
    }

    Ok(translations)
}

/// Locates the first balanced JSON object in a model response.
///
/// A provider's JSON mode constrains the shape of the object, not the whole
/// message: models still wrap it in a fence, prefix it with a sentence, or —
/// as a reasoning model does when its thinking lands in the same field as the
/// answer — keep reasoning aloud after the closing brace and revise the
/// translation in prose. The object itself stays parseable in every one of
/// those cases, so the boundary this function defines is exactly the boundary
/// the parser needs: the object is recovered, anything around it is not
/// trusted. Scaffolding that never closes is left for the caller to reject.
///
/// Braces and quotes inside string literals are not structure. Translated
/// dialogue routinely contains both, including unbalanced ones from a
/// truncated or degraded `text` value, so a naive depth count would end the
/// object mid-value and hand the parser a fragment.
fn json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0_usize;
    let mut in_string = false;
    let mut escaped = false;

    for (offset, byte) in text.as_bytes()[start..].iter().copied().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + offset + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Keeps a model response readable in a log line without truncating so hard
/// that the shape of the failure is lost.
///
/// Responses are usually pretty-printed, so line breaks and other control
/// characters are escaped rather than passed through: the snippet is formatted
/// into an error context, and one failure should stay one line.
pub(crate) fn snippet(text: &str) -> String {
    const LIMIT: usize = 2000;
    let trimmed = text.trim();
    let (visible, elided) = match trimmed.char_indices().nth(LIMIT) {
        Some((end, _)) => (&trimmed[..end], true),
        None => (trimmed, false),
    };

    let mut output = String::with_capacity(visible.len());
    for character in visible.chars() {
        match character {
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            control if control.is_control() => {
                let _ = write!(output, "\\u{{{:04x}}}", control as u32);
            }
            character => output.push(character),
        }
    }
    if elided {
        let _ = write!(output, "… ({} bytes total)", trimmed.len());
    }
    output
}

pub(crate) fn output_schema(expected: usize) -> Value {
    json!({
        "type": "object",
        "properties": {
            "translations": {
                "type": "array",
                "minItems": expected,
                "maxItems": expected,
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "integer",
                            "minimum": 0,
                            "maximum": expected.saturating_sub(1),
                            "description": "The ID copied from the corresponding input segment."
                        },
                        "text": {
                            "type": "string",
                            "description": "The translation of the input segment with this ID."
                        }
                    },
                    "required": ["id", "text"],
                    "additionalProperties": false
                }
            }
        },
        "required": ["translations"],
        "additionalProperties": false
    })
}

/// JSON scaffolding the response spends on each translated segment:
/// `{"id":`, the id, `,`, `"text":`, the quoted value, `}`, and a separator with
/// indentation. Sixteen covers a model that pads the value as well.
const RESPONSE_TOKENS_PER_SEGMENT: usize = 16;

/// Worst-case response tokens per source character.
///
/// A CJK source already runs about one token per character, codepoints outside
/// the vocabulary fall back to byte pieces, and translating into an
/// alphabetic language lengthens the text before it is tokenized, so the
/// response is not bounded by the token count of its own source. Two leaves
/// headroom over those cases: the ceiling only has to be exceeded by a runaway
/// generation, so paying for slack here is cheaper than paying for a truncated
/// page, which loses every segment on that page at once.
const RESPONSE_TOKENS_PER_SOURCE_CHAR: usize = 2;

/// Floor for the derived budget.
///
/// Reasoning models emit a thinking preamble before the JSON and draw it from
/// the same budget, so a page whose segments are nearly empty still has to pay
/// for the preamble plus the response envelope.
const MIN_RESPONSE_TOKENS: usize = 1024;

/// Ceiling for the derived budget.
///
/// The local backend sizes its context as `prompt + max_tokens`, so this bound
/// is a memory bound as much as a truncation bound: without it, an oversized
/// request silently turns into an oversized context allocation. Pages are
/// sliced before they reach the translator, so a page needing more than this is
/// a caller that bypassed that boundary rather than a page that ran out of
/// room, and the budget clamps instead of growing without limit.
const MAX_RESPONSE_TOKENS: usize = 8192;

/// Token budget for one translation response, derived from the input scale.
///
/// The response is a JSON object holding exactly one `{"id","text"}` pair per
/// input segment (see `output_schema`), so the requirement is the translated
/// text plus the per-segment envelope above. Deriving the budget from the same
/// segments the schema is built from keeps the two consistent: the schema
/// promises one entry per segment, and this promises room for that many
/// entries. A fixed ceiling truncates a dense page into unparsable JSON, and
/// because a truncated response fails the whole page, under-provisioning costs
/// every segment on it.
#[must_use]
pub(crate) fn response_budget(source_segments: &[String]) -> u32 {
    let source_chars = source_segments
        .iter()
        .map(|segment| segment.chars().count())
        .sum::<usize>();
    let required = MIN_RESPONSE_TOKENS
        .saturating_add(source_chars.saturating_mul(RESPONSE_TOKENS_PER_SOURCE_CHAR))
        .saturating_add(
            source_segments
                .len()
                .saturating_mul(RESPONSE_TOKENS_PER_SEGMENT),
        )
        .min(MAX_RESPONSE_TOKENS);
    u32::try_from(required).unwrap_or(u32::MAX)
}

fn translation_system_prompt(request: &TranslationRequest) -> String {
    let source = request
        .source_language
        .map(|language| language.to_string())
        .unwrap_or_else(|| "the detected source language".to_owned());
    let mut prompt = format!(
        indoc! {"
            You are a professional manga translator.

            Translation requirements:
            - Translate every input segment from {source} into natural {target}.
            - Preserve meaning, character voice, emotional tone, relationship nuance, emphasis, and sound effects.
            - Localize idioms and sound effects naturally while keeping wording concise enough for speech bubbles.
            - Use surrounding segments only for disambiguation and continuity; never merge or split segments.
            - The input text comes from OCR and may contain misread characters, dropped characters, or garbled text. Read each segment as the phrase it is most plausibly meant to be, using the neighbouring segments; where a plausible reading exists, prefer it over a literal rendering of obvious OCR noise.
            - Write every translated `text` value only in {target}; do not include source text, notes, explanations, or alternatives.
            - Never preserve or repeat original-language text; translate names, terms, and sound effects using natural {target} conventions.

            Output requirements:
            - Each input segment has a numeric `id`.
            - Return only a JSON object whose `translations` array contains exactly one object per input segment, each carrying its own `id` and the translated `text`.
            - Copy every input ID exactly once; order does not matter.
            - Never merge, split, omit, duplicate, or add segments.
        "},
        source = source,
        target = request.target_language,
    )
    .trim_end()
    .to_owned();

    if !request.context.is_empty() {
        prompt.push_str("\n\n");
        prompt.push_str(indoc! {"
            Context requirements:
            Use the supplied context only to preserve terminology, character voice, and dialogue continuity.
            Do not translate or return the context entries.
        "}.trim_end());
    }

    if request.image.is_some() {
        prompt.push_str("\n\n");
        prompt.push_str(indoc! {"
            Image requirements:
            Use the attached original page image as visual context for speaker identity, tone, layout, and ambiguous OCR.

            The input text comes from OCR and may disagree with the page. When a supplied segment clearly differs from the writing inside its own bubble, read it from the page and translate what the page says.

            Text that belongs to a different bubble or element is not in scope: leave it out and do not add segments for it. Translate only the supplied segments.
        "}.trim_end());
    }

    if let Some(instructions) = request
        .instructions
        .as_deref()
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty())
    {
        prompt.push_str("\n\nAdditional instructions:\n");
        prompt.push_str(instructions);
    }
    prompt
}

#[derive(Serialize)]
struct TranslationInput<'a> {
    source_language: Option<Language>,
    target_language: Language,
    context: &'a [TranslationContext],
    segments: Vec<TranslationInputSegment<'a>>,
}

#[derive(Serialize)]
struct TranslationInputSegment<'a> {
    id: usize,
    text: &'a str,
}

#[derive(Debug, Deserialize)]
struct TranslationOutput {
    translations: Vec<TranslationOutputSegment>,
}

#[derive(Debug, Deserialize)]
struct TranslationOutputSegment {
    #[serde(deserialize_with = "deserialize_segment_id")]
    id: usize,
    text: String,
}

fn deserialize_segment_id<'de, D>(deserializer: D) -> Result<usize, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SegmentId {
        Number(usize),
        String(String),
    }

    match SegmentId::deserialize(deserializer)? {
        SegmentId::Number(id) => Ok(id),
        SegmentId::String(id) => id.trim().parse().map_err(de::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippet_keeps_a_response_on_one_line() {
        let flattened = snippet("{\n  \"translations\": [\n\t\"hello\"\n  ]\n}");

        assert!(!flattened.contains('\n'), "{flattened}");
        assert!(!flattened.contains('\t'), "{flattened}");
        assert_eq!(flattened, r#"{\n  "translations": [\n\t"hello"\n  ]\n}"#);
    }

    #[test]
    fn snippet_escapes_other_control_characters() {
        assert_eq!(snippet("before\u{7}after"), r"before\u{0007}after");
    }

    #[test]
    fn snippet_reports_the_length_it_elided() {
        let flattened = snippet(&"x".repeat(2_500));

        assert!(flattened.starts_with(&"x".repeat(2_000)));
        assert!(
            flattened.ends_with("\u{2026} (2500 bytes total)"),
            "{flattened}"
        );
    }

    #[test]
    fn parses_plain_json() {
        let source = ["one".to_owned(), "two".to_owned()];
        let response = r#"{"translations":[{"id":0,"text":"hello"},{"id":1,"text":"world"}]}"#;

        assert_eq!(
            translations("test", response, &source).unwrap(),
            ["hello", "world"]
        );
    }

    #[test]
    fn rejects_malformed_and_truncated_json() {
        let source = ["one".to_owned(), "two".to_owned()];
        for response in [
            r#"{translations: [{id: 0, text: 'hello'}, {id: 1, text: 'world'},],}"#,
            "{\"translations\":[{\"id\":0,\"text\":\"hello\"},{\"id\":1,\"text\":\"world\"",
        ] {
            assert!(
                translations("test", response, &source).is_err(),
                "{response}"
            );
        }
    }

    #[test]
    fn recovers_the_object_from_a_surrounded_response() {
        // The failure this covers: a reasoning model emits a complete object
        // and then keeps thinking in the same field, so the trailing prose is
        // what made the whole response unparseable.
        let source = ["one".to_owned(), "two".to_owned()];
        let response = concat!(
            "{\"translations\":\n",
            "[{\"id\":0,\"text\":\"hello\"},{\"id\":1,\"text\":\"world\"}]}\n\n",
            "Wait, I need to double-check this input.\n\nFinal:\n",
            "hello / world"
        );

        assert_eq!(
            translations("test", response, &source).unwrap(),
            ["hello", "world"]
        );
    }

    #[test]
    fn recovers_the_object_from_a_fenced_response() {
        let source = ["one".to_owned(), "two".to_owned()];
        let response = "```json\n{\"translations\":[{\"id\":0,\"text\":\"hello\"},{\"id\":1,\"text\":\"world\"}]}\n```";

        assert_eq!(
            translations("test", response, &source).unwrap(),
            ["hello", "world"]
        );
    }

    #[test]
    fn object_extraction_ends_on_the_brace_that_balances_the_start() {
        // Dialogue contains quotes, backslashes, and braces of its own; a
        // count that ignored string literals would stop inside this value and
        // leave the caller with a fragment.
        let text = r#"{"translations":[{"id":0,"text":"a \"quoted\" {note} C:\\ path"}]}"#;

        assert_eq!(
            json_object(text),
            Some(r#"{"translations":[{"id":0,"text":"a \"quoted\" {note} C:\\ path"}]}"#)
        );
    }

    #[test]
    fn object_extraction_reports_nothing_for_an_unclosed_response() {
        assert_eq!(json_object(r#"{"translations":[{"id":0,"text":"hel"#), None);
        assert_eq!(json_object("no object here"), None);
    }

    #[test]
    fn an_unclosed_response_still_reports_the_original_failure() {
        // Falling back to the whole response keeps the error anchored to what
        // actually went wrong instead of claiming the object was never there.
        let source = ["one".to_owned()];
        let error = format!(
            "{:#}",
            translations("test", r#"{"translations":[{"id":0,"text":"hel"#, &source)
                .expect_err("truncated text should fail")
        );

        assert!(error.contains("EOF while parsing a string"), "{error}");
    }

    #[test]
    fn parse_error_preserves_the_root_failure_and_response() {
        let source = ["one".to_owned()];
        let response = "{\n  \"translations\": [{\"id\": 0}]\n}";
        let error = format!(
            "{:#}",
            translations("test", response, &source).expect_err("missing text should fail")
        );

        assert!(error.contains("missing field `text`"), "{error}");
        assert!(
            error.contains(r#"response was: {\n  "translations": [{"id": 0}]\n}"#),
            "{error}"
        );
    }

    #[test]
    fn restores_input_order_from_ids() {
        let source = ["one".to_owned(), "two".to_owned()];
        let response = r#"{"translations":[{"id":1,"text":"world"},{"id":0,"text":"hello"}]}"#;
        assert_eq!(
            translations("test", response, &source).unwrap(),
            ["hello", "world"]
        );
    }

    #[test]
    fn tolerates_duplicate_missing_and_out_of_range_ids() {
        let source = ["one".to_owned(), "two".to_owned()];
        let short = r#"{"translations":[{"id":1,"text":"world"}]}"#;
        assert_eq!(
            translations("test", short, &source).unwrap(),
            ["one", "world"]
        );

        let response = concat!(
            r#"{"translations":["#,
            r#"{"id":0,"text":"hello"},"#,
            r#"{"id":0,"text":"duplicate"},"#,
            r#"{"id":9,"text":"extra"}"#,
            "]}"
        );
        assert_eq!(
            translations("test", response, &source).unwrap(),
            ["hello", "two"]
        );
    }

    #[test]
    fn prompt_payload_contains_ordered_context() {
        let request = TranslationRequest::new(["new"], Language::English)
            .with_context([TranslationContext::new("old", "previous")]);
        let (_, user) = prompts(&request).unwrap();
        let input: serde_json::Value = serde_json::from_str(&user).unwrap();
        assert_eq!(input["context"][0]["source"], "old");
        assert_eq!(input["context"][0]["translation"], "previous");
        assert_eq!(input["segments"][0]["id"], 0);
        assert_eq!(input["segments"][0]["text"], "new");
    }

    #[test]
    fn system_prompt_encodes_invariants_and_custom_instructions() {
        let request = TranslationRequest::new(["hello"], Language::Korean)
            .with_source_language(Language::Japanese)
            .with_instructions("Use informal speech.");
        let prompt = translation_system_prompt(&request);
        assert!(prompt.contains("from Japanese into natural Korean"));
        assert!(prompt.contains("Copy every input ID exactly once"));
        assert!(prompt.contains("Use informal speech."));
    }

    #[test]
    fn schema_requires_the_expected_number_of_id_text_pairs() {
        let schema = output_schema(3);
        let translations = &schema["properties"]["translations"];
        assert_eq!(translations["minItems"], 3);
        assert_eq!(translations["maxItems"], 3);
        assert_eq!(translations["items"]["properties"]["id"]["minimum"], 0);
        assert_eq!(translations["items"]["properties"]["id"]["maximum"], 2);
        assert_eq!(translations["items"]["additionalProperties"], false);
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn response_budget_charges_the_json_envelope_per_segment() {
        // Same characters, different shapes: the envelope scales with the number
        // of entries the schema admits, not with the page's character count.
        let page = vec!["x".repeat(100); 5];
        let characters = 500 * RESPONSE_TOKENS_PER_SOURCE_CHAR;

        assert_eq!(
            response_budget(&["x".repeat(500)]) as usize,
            MIN_RESPONSE_TOKENS + characters + RESPONSE_TOKENS_PER_SEGMENT
        );
        assert_eq!(
            response_budget(&page) as usize,
            MIN_RESPONSE_TOKENS + characters + 5 * RESPONSE_TOKENS_PER_SEGMENT
        );
    }

    #[test]
    fn response_budget_keeps_room_for_a_reasoning_preamble() {
        // A page of near-empty segments still has to cover the preamble and the
        // array envelope, so the floor holds regardless of how little there is
        // to translate.
        assert_eq!(
            response_budget(&vec![String::new(); 4]) as usize,
            MIN_RESPONSE_TOKENS + 4 * RESPONSE_TOKENS_PER_SEGMENT
        );
    }

    #[test]
    fn response_budget_clamps_an_input_larger_than_a_page() {
        assert_eq!(
            response_budget(&vec!["x".repeat(100_000); 64]) as usize,
            MAX_RESPONSE_TOKENS
        );
    }

    #[test]
    fn response_budget_counts_characters_rather_than_bytes() {
        // CJK source is three UTF-8 bytes per character; budgeting on bytes
        // would triple the ceiling for the same page.
        assert_eq!(
            response_budget(&["せ".repeat(200)]),
            response_budget(&["x".repeat(200)])
        );
    }

    #[test]
    fn empty_custom_instructions_are_ignored() {
        let request = TranslationRequest::new(["hello"], Language::English).with_instructions("  ");
        assert!(!translation_system_prompt(&request).contains("Additional instructions"));
    }

    #[test]
    fn context_is_reference_only() {
        let request = TranslationRequest::new(["Where is she?"], Language::Japanese)
            .with_context([TranslationContext::new("I saw Alice.", "アリスを見た。")]);
        let prompt = translation_system_prompt(&request);
        assert!(prompt.contains("dialogue continuity"));
        assert!(prompt.contains("Do not translate or return the context"));
    }

    #[test]
    fn image_context_does_not_expand_the_translation_scope() {
        let request = TranslationRequest::new(["text"], Language::English)
            .with_image(std::sync::Arc::new(image::DynamicImage::new_rgb8(1, 1)));
        let prompt = translation_system_prompt(&request);
        assert!(prompt.contains("attached original page image"));
        assert!(prompt.contains("Translate only the supplied segments"));
    }

    #[test]
    fn the_image_may_correct_a_segment_without_adding_one() {
        // 这两件事必须同时说清：条内按图纠错是允许的，条外增补仍然禁止。少了前者模型不敢纠正
        // 错字，少了后者它会借纠错之名把整页文字都翻出来。
        let request = TranslationRequest::new(["text"], Language::English)
            .with_image(std::sync::Arc::new(image::DynamicImage::new_rgb8(1, 1)));
        let prompt = translation_system_prompt(&request);

        assert!(prompt.contains("may disagree with the page"));
        assert!(prompt.contains("inside its own bubble"));
        assert!(prompt.contains("translate what the page says"));
        assert!(prompt.contains("do not add segments for it"));
    }

    #[test]
    fn ocr_noise_may_be_read_through_without_waiting_for_an_image() {
        // 纠错引导不能只挂在图片那一段：纯文本服务商同样拿到有瑕疵的 OCR 结果，而它们看不到图。
        let request = TranslationRequest::new(["text"], Language::English);
        let prompt = translation_system_prompt(&request);

        assert!(prompt.contains("comes from OCR and may contain misread characters"));
        assert!(prompt.contains("using the neighbouring segments"));
        assert!(!prompt.contains("Image requirements"), "no image, no image section");
    }

    #[test]
    fn the_output_contract_counts_every_segment_exactly_once() {
        // json_object 那一档拿不到 minItems/maxItems，段数全靠这两句，所以措辞要能扛住漏返回。
        let prompt = translation_system_prompt(&TranslationRequest::new(["a"], Language::English));
        assert!(prompt.contains("contains exactly one object per input segment"));
        assert!(prompt.contains("Copy every input ID exactly once"));
    }
}
