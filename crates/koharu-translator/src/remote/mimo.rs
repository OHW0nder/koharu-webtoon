// https://mimo.mi.com/docs/zh-CN/quick-start/summary/first-api-call
// https://mimo.mi.com/docs/zh-CN/quick-start/usage-guide/multimodal-understanding/image-understanding
// https://mimo.mi.com/docs/zh-CN/quick-start/usage-guide/text-generation/structured-output
// https://mimo.mi.com/docs/zh-CN/quick-start/usage-guide/text-generation/deep-thinking

use anyhow::Context;
use koharu_secrets::ExposeSecret;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use url::Url;

use super::send_json;
use crate::{
    GenerationConfig, Model, Provider, Result, TranslationRequest, backend::encode_image,
    display_name, prompt,
};

/// Pay-as-you-go endpoint. Token Plan subscribers get a dedicated base URL from
/// the console, which is why this is configurable rather than pinned.
const DEFAULT_BASE_URL: &str = "https://api.xiaomimimo.com/v1";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(default)]
pub struct MiMoConfig {
    pub base_url: Option<Url>,
}

impl Default for MiMoConfig {
    fn default() -> Self {
        Self {
            base_url: Some(
                Url::parse(DEFAULT_BASE_URL).expect("default MiMo URL is valid"),
            ),
        }
    }
}

pub(super) async fn models(client: &Client, config: &MiMoConfig) -> Result<Vec<Model>> {
    let Some(api_key) = koharu_secrets::get("mimo")? else {
        return Ok(Vec::new());
    };
    let response: ModelsResponse = send_json(
        "mimo",
        client
            .get(endpoint(config.base_url.as_ref(), "models"))
            .bearer_auth(api_key.expose_secret()),
    )
    .await?;
    Ok(response
        .data
        .into_iter()
        .filter(|model| supports_translation(&model.id))
        .map(|model| Model {
            provider: Provider::MiMo,
            name: display_name(&model.id),
            model: Some(model.id),
            quantizations: Vec::new(),
            vision: true,
            reasoning: true,
        })
        .collect())
}

/// The V2.6 family, and only that family.
///
/// The image-understanding and structured-output guides both name
/// `mimo-v2.6-flash`, `mimo-v2.6-pro` and `mimo-v2.6-pro-ultraspeed` as the
/// models that accept images and JSON mode. The catalogue also carries the
/// superseded V2.5 text models and the V2.5 speech models, which have no chat
/// translation contract at all.
fn supports_translation(id: &str) -> bool {
    id.to_ascii_lowercase().starts_with("mimo-v2.6")
}

/// Sends one translation request.
///
/// Every V2.6 model is natively multimodal, so the page image rides along in
/// the user message whenever the caller enabled vision. Sampling is left to the
/// service unless the caller set it, matching every other hosted provider.
pub(super) async fn translate(
    client: &Client,
    config: &MiMoConfig,
    model: &str,
    generation: &GenerationConfig,
    request: &TranslationRequest,
) -> Result<Vec<String>> {
    let api_key = koharu_secrets::get("mimo")?.context("mimo API key is not configured")?;
    let (system, user) = prompt::prompts(request)?;
    let user_content = match request.image.as_deref() {
        Some(image) => MessageContent::Parts(vec![
            ContentPart::Text { text: user },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: encode_image(image)?.data_url(),
                },
            },
        ]),
        None => MessageContent::Text(user),
    };
    let body = ChatRequest {
        model,
        messages: [
            Message {
                role: "system",
                content: MessageContent::Text(system),
            },
            Message {
                role: "user",
                content: user_content,
            },
        ],
        thinking: generation.reasoning.map(|enabled| ThinkingConfig {
            kind: if enabled { "enabled" } else { "disabled" },
        }),
        max_completion_tokens: generation.max_tokens,
        response_format: ResponseFormat {
            kind: "json_object",
        },
        temperature: generation.temperature,
        top_p: generation.top_p,
    };
    let response: ChatResponse = send_json(
        "mimo",
        client
            .post(endpoint(config.base_url.as_ref(), "chat/completions"))
            .bearer_auth(api_key.expose_secret())
            .json(&body),
    )
    .await?;
    let text = response
        .choices
        .into_iter()
        .next()
        .context("MiMo returned no choices")?
        .message
        .content
        .context("MiMo returned no message content")?;
    Ok(prompt::translations("mimo", &text, &request.segments)?)
}

fn endpoint(base_url: Option<&Url>, suffix: &str) -> String {
    let base_url = base_url.map_or(DEFAULT_BASE_URL, Url::as_str);
    format!(
        "{}/{}",
        base_url.trim_end_matches('/'),
        suffix.trim_start_matches('/')
    )
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [Message; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    response_format: ResponseFormat,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
}

#[derive(Serialize)]
struct ThinkingConfig {
    #[serde(rename = "type")]
    kind: &'static str,
}

/// MiMo exposes JSON mode only. The shape of the object comes from the system
/// prompt, which spells out the `translations` array and its per-segment
/// `id`/`text` pair, so the schema is not restated as a request constraint.
#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct Message {
    role: &'static str,
    content: MessageContent,
}

#[derive(Serialize)]
#[serde(untagged)]
enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Serialize)]
struct ImageUrl {
    url: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct ModelsResponse {
    data: Vec<ListedModel>,
}

#[derive(Deserialize)]
struct ListedModel {
    id: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(model: &'static str) -> ChatRequest<'static> {
        ChatRequest {
            model,
            messages: [
                Message {
                    role: "system",
                    content: MessageContent::Text("system".to_owned()),
                },
                Message {
                    role: "user",
                    content: MessageContent::Text("user".to_owned()),
                },
            ],
            thinking: Some(ThinkingConfig { kind: "disabled" }),
            max_completion_tokens: Some(1024),
            response_format: ResponseFormat {
                kind: "json_object",
            },
            temperature: None,
            top_p: None,
        }
    }

    #[test]
    fn endpoint_keeps_the_v1_path_of_the_configured_base_url() {
        let url = Url::parse("https://token-plan-cn.xiaomimimo.com/v1/").unwrap();
        assert_eq!(
            endpoint(Some(&url), "chat/completions"),
            "https://token-plan-cn.xiaomimimo.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint(None, "models"),
            "https://api.xiaomimimo.com/v1/models"
        );
    }

    #[test]
    fn serializes_the_mimo_request_contract() {
        let value = serde_json::to_value(request("mimo-v2.6-pro")).unwrap();

        assert_eq!(value["thinking"]["type"], "disabled");
        assert_eq!(value["max_completion_tokens"], 1024);
        // MiMo renamed this away from `max_tokens`.
        assert!(value.get("max_tokens").is_none());
        assert_eq!(value["response_format"]["type"], "json_object");
        // JSON mode is the only structured-output contract MiMo documents, so a
        // schema block here would be rejected rather than merely ignored.
        assert!(value["response_format"].get("json_schema").is_none());
    }

    #[test]
    fn thinking_is_omitted_when_the_model_cannot_be_asked_for_it() {
        let mut body = request("mimo-v2.6-flash");
        body.thinking = None;
        let value = serde_json::to_value(body).unwrap();

        assert!(value.get("thinking").is_none());
    }

    #[test]
    fn a_vision_request_keeps_the_image_inside_the_user_message() {
        let mut body = request("mimo-v2.6-pro-ultraspeed");
        body.messages[1].content = MessageContent::Parts(vec![
            ContentPart::Text {
                text: "translate".to_owned(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/jpeg;base64,AAAA".to_owned(),
                },
            },
        ]);
        let value = serde_json::to_value(body).unwrap();

        assert_eq!(value["messages"][0]["content"], "system");
        assert_eq!(value["messages"][1]["content"][0]["type"], "text");
        assert_eq!(value["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(
            value["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,AAAA"
        );
    }

    #[test]
    fn every_v26_model_translates_and_the_rest_of_the_catalogue_does_not() {
        // The whole V2.6 family is natively multimodal, so vision is not
        // narrowed per model here.
        for id in [
            "mimo-v2.6-pro",
            "mimo-v2.6-flash",
            "mimo-v2.6-pro-ultraspeed",
        ] {
            assert!(supports_translation(id), "{id} translates");
        }
        for id in [
            "mimo-v2.5",
            "mimo-v2.5-pro",
            "mimo-v2.5-asr",
            "mimo-v2.5-tts",
        ] {
            assert!(!supports_translation(id), "{id} is out of the family");
        }
    }
}