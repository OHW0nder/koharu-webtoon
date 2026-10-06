// https://api-docs.deepseek.com/api/create-chat-completion
// https://api-docs.deepseek.com/api/list-models
// https://api-docs.deepseek.com/guides/json_mode
// https://api-docs.deepseek.com/guides/thinking_mode

use anyhow::Context;
use koharu_secrets::ExposeSecret;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::send_json;
use crate::{
    GenerationConfig, Model, Provider, Result, TranslationRequest, backend::encode_image,
    display_name, prompt,
};

const URL: &str = "https://api.deepseek.com/chat/completions";
const MODELS_URL: &str = "https://api.deepseek.com/models";

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(default)]
pub struct DeepSeekConfig {}

pub(super) async fn models(client: &Client) -> Result<Vec<Model>> {
    let Some(api_key) = koharu_secrets::get("deepseek")? else {
        return Ok(Vec::new());
    };
    let response: ModelsResponse = send_json(
        "deepseek",
        client.get(MODELS_URL).bearer_auth(api_key.expose_secret()),
    )
    .await?;
    Ok(response
        .data
        .into_iter()
        .map(|model| Model {
            provider: Provider::DeepSeek,
            name: display_name(&model.id),
            vision: supports_vision(&model.id),
            model: Some(model.id),
            quantizations: Vec::new(),
            reasoning: true,
        })
        .collect())
}

/// 图像输入由 flash 系列承接。
///
/// 官方 Vision 指南只声明 flash 系列接受图像，而 `deepseek-chat` 与
/// `deepseek-reasoner` 是 flash 两种思考模式的旧别名，同样接受图像。pro 与
/// coder 系列不出现在该页面上，按纯文本处理。
///
/// 未列出的新模型一律判为不支持：多发一张图会让整页翻译失败，而少发一张图
/// 只是少一层上下文，两者不对等。
fn supports_vision(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.contains("flash") || id.ends_with("-chat") || id.ends_with("-reasoner")
}

/// Sends one translation request.
///
/// Sampling is left to DeepSeek unless the caller set it, matching every other
/// hosted provider. A provider-level temperature floor belongs to the model
/// rather than here: this one existed to lift creativity, which is the wrong
/// trade for a response that has to satisfy `response_format` and loses the
/// whole page when a single segment degrades.
pub(super) async fn translate(
    client: &Client,
    _config: &DeepSeekConfig,
    model: &str,
    generation: &GenerationConfig,
    request: &TranslationRequest,
) -> Result<Vec<String>> {
    let api_key = koharu_secrets::get("deepseek")?.context("deepseek API key is not configured")?;
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
        temperature: generation.temperature,
        top_p: generation.top_p,
        max_tokens: generation.max_tokens,
        thinking: generation.reasoning.map(|enabled| ThinkingConfig {
            kind: if enabled { "enabled" } else { "disabled" },
        }),
        response_format: ResponseFormat {
            kind: "json_object",
        },
    };
    let response: ChatResponse = send_json(
        "deepseek",
        client
            .post(URL)
            .bearer_auth(api_key.expose_secret())
            .json(&body),
    )
    .await?;
    let text = response
        .choices
        .into_iter()
        .next()
        .context("DeepSeek returned no choices")?
        .message
        .content
        .context("DeepSeek returned no message content")?;
    Ok(prompt::translations("deepseek", &text, &request.segments)?)
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: [Message; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<ThinkingConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
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

    #[test]
    fn serializes_deepseek_request_contract() {
        let body = ChatRequest {
            model: "deepseek-v4-pro",
            messages: [
                Message {
                    role: "system",
                    content: MessageContent::Text("return json".to_owned()),
                },
                Message {
                    role: "user",
                    content: MessageContent::Text("translate".to_owned()),
                },
            ],
            thinking: Some(ThinkingConfig { kind: "disabled" }),
            max_tokens: Some(1024),
            response_format: ResponseFormat {
                kind: "json_object",
            },
            temperature: None,
            top_p: None,
        };
        let value = serde_json::to_value(body).unwrap();

        assert_eq!(value["thinking"]["type"], "disabled");
        assert_eq!(value["max_tokens"], 1024);
        assert_eq!(value["response_format"]["type"], "json_object");
        assert!(value.get("json_schema").is_none());
        assert!(value.get("frequency_penalty").is_none());
        assert!(value.get("presence_penalty").is_none());
    }

    #[test]
    fn flash_models_take_images_and_pro_models_do_not() {
        // 视觉能力挂在 flash 系列上，两个旧名是它的思考模式别名。
        for id in [
            "deepseek-flash",
            "deepseek-v4-flash",
            "deepseek-v4-flash-vision-exp",
            "deepseek-chat",
            "deepseek-reasoner",
        ] {
            assert!(supports_vision(id), "{id} takes images");
        }
        for id in ["deepseek-v4-pro", "deepseek-coder", "deepseek-v4"] {
            assert!(!supports_vision(id), "{id} is text only");
        }
    }

    #[test]
    fn a_vision_request_keeps_the_image_inside_the_user_message() {
        // DeepSeek returns HTTP 400 when an image block reaches the system
        // message, so the text-only system prompt is part of the contract
        // rather than an optimization.
        let body = ChatRequest {
            model: "deepseek-flash",
            messages: [
                Message {
                    role: "system",
                    content: MessageContent::Text("return json".to_owned()),
                },
                Message {
                    role: "user",
                    content: MessageContent::Parts(vec![
                        ContentPart::Text {
                            text: "translate".to_owned(),
                        },
                        ContentPart::ImageUrl {
                            image_url: ImageUrl {
                                url: "data:image/jpeg;base64,AAAA".to_owned(),
                            },
                        },
                    ]),
                },
            ],
            thinking: Some(ThinkingConfig { kind: "disabled" }),
            max_tokens: Some(1024),
            response_format: ResponseFormat {
                kind: "json_object",
            },
            temperature: None,
            top_p: None,
        };
        let value = serde_json::to_value(body).unwrap();

        assert_eq!(value["messages"][0]["content"], "return json");
        assert_eq!(value["messages"][1]["content"][0]["type"], "text");
        assert_eq!(value["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(
            value["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/jpeg;base64,AAAA"
        );
    }

    #[test]
    fn sampling_is_left_to_the_service_unless_the_caller_sets_it() {
        let body = |temperature| ChatRequest {
            model: "deepseek-chat",
            messages: [
                Message {
                    role: "system",
                    content: MessageContent::Text("return json".to_owned()),
                },
                Message {
                    role: "user",
                    content: MessageContent::Text("translate".to_owned()),
                },
            ],
            thinking: None,
            max_tokens: None,
            response_format: ResponseFormat {
                kind: "json_object",
            },
            temperature,
            top_p: None,
        };

        // A floor here would override the caller on every request and had no
        // counterpart at any other provider.
        assert!(serde_json::to_value(body(None)).unwrap().get("temperature").is_none());
        assert_eq!(
            serde_json::to_value(body(Some(0.2))).unwrap()["temperature"].as_f64(),
            Some(0.2_f32 as f64)
        );
    }
}
