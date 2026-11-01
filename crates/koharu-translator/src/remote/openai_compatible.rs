// https://developers.openai.com/api/reference/resources/chat/subresources/completions/methods/create

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

const DEFAULT_BASE_URL: &str = "http://localhost:11434/v1";

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, specta::Type)]
#[serde(default)]
pub struct OpenAiCompatibleConfig {
    pub base_url: Option<Url>,
}

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            base_url: Some(
                Url::parse(DEFAULT_BASE_URL).expect("default OpenAI-compatible URL is valid"),
            ),
        }
    }
}

pub(super) async fn translate(
    client: &Client,
    config: &OpenAiCompatibleConfig,
    model: &str,
    generation: &GenerationConfig,
    request: &TranslationRequest,
) -> Result<Vec<String>> {
    let api_key = koharu_secrets::get("openai-compatible")?;
    let body = ChatRequest::new(model, generation, request)?;
    let http = client
        .post(endpoint(config.base_url.as_ref(), "chat/completions"))
        .json(&body);
    let http = match api_key {
        Some(api_key) => http.bearer_auth(api_key.expose_secret()),
        None => http,
    };
    let response: ChatResponse = send_json("openai-compatible", http).await?;
    let text = response
        .choices
        .into_iter()
        .next()
        .context("OpenAI-compatible backend returned no choices")?
        .message
        .content
        .context("OpenAI-compatible backend returned no message content")?;
    Ok(prompt::translations(
        "openai-compatible",
        &text,
        &request.segments,
    )?)
}

pub(super) async fn models(client: &Client, config: &OpenAiCompatibleConfig) -> Result<Vec<Model>> {
    let api_key = koharu_secrets::get("openai-compatible")?;
    let request = client.get(endpoint(config.base_url.as_ref(), "models"));
    let request = match api_key {
        Some(api_key) => request.bearer_auth(api_key.expose_secret()),
        None => request,
    };
    let response: ModelsResponse = send_json("openai-compatible", request).await?;
    Ok(response
        .data
        .into_iter()
        .map(|model| Model {
            provider: Provider::OpenAiCompatible,
            name: display_name(&model.id),
            model: Some(model.id),
            quantizations: Vec::new(),
            vision: true,
            reasoning: true,
        })
        .collect())
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
    stream: bool,
    messages: [Message; 2],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    frequency_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    presence_penalty: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'static str>,
    response_format: ResponseFormat,
}

impl<'a> ChatRequest<'a> {
    fn new(
        model: &'a str,
        generation: &GenerationConfig,
        request: &TranslationRequest,
    ) -> Result<Self> {
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
        Ok(Self {
            model,
            stream: false,
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
            frequency_penalty: generation.frequency_penalty,
            presence_penalty: generation.presence_penalty,
            reasoning_effort: generation.reasoning.map(|enabled| {
                let id = model.rsplit('/').next().unwrap_or(model);
                if id.starts_with("gemini-") {
                    // Gemini 3.8 cannot disable thinking, and older Pro models reject medium.
                    // Use levels shared by both, including provider-prefixed proxy model IDs.
                    if enabled { "high" } else { "low" }
                } else if enabled {
                    "medium"
                } else {
                    "none"
                }
            }),
            response_format: ResponseFormat {
                kind: "json_schema",
                json_schema: JsonSchema {
                    name: "manga_translation",
                    strict: true,
                    schema: prompt::output_schema(request.segments.len()),
                },
            },
        })
    }
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: &'static str,
    json_schema: JsonSchema,
}

#[derive(Serialize)]
struct JsonSchema {
    name: &'static str,
    strict: bool,
    schema: serde_json::Value,
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
    fn gemini_thinking_uses_supported_levels_in_the_outgoing_request() {
        let request = TranslationRequest::new(["Hello"], crate::Language::Vietnamese);
        for model in [
            "gemini-3.8-flash",
            "ag/gemini-3.8-flash-high",
            "ag/gemini-3.7-flash-low",
            "google/gemini-3-pro-preview",
            "ag/gemini-4-argon-medium",
        ] {
            for (reasoning, expected) in [
                (Some(false), Some("low")),
                (Some(true), Some("high")),
                (None, None),
            ] {
                let generation = GenerationConfig {
                    reasoning,
                    ..GenerationConfig::default()
                };
                let body =
                    serde_json::to_value(ChatRequest::new(model, &generation, &request).unwrap())
                        .unwrap();
                assert_eq!(
                    body.get("reasoning_effort")
                        .and_then(serde_json::Value::as_str),
                    expected,
                    "{model} reasoning={reasoning:?}"
                );
                assert_eq!(body["model"], model);
                assert_eq!(body["response_format"]["json_schema"]["strict"], true);
            }
        }
        for (reasoning, expected) in [
            (Some(false), Some("none")),
            (Some(true), Some("medium")),
            (None, None),
        ] {
            let generation = GenerationConfig {
                reasoning,
                ..GenerationConfig::default()
            };
            let body = serde_json::to_value(
                ChatRequest::new("another-model", &generation, &request).unwrap(),
            )
            .unwrap();
            assert_eq!(
                body.get("reasoning_effort")
                    .and_then(serde_json::Value::as_str),
                expected
            );
        }
    }

    #[tokio::test]
    #[ignore = "manual acceptance: sends eleven synthetic dialogue segments and reference context through the configured OS credential to a local proxy"]
    async fn local_gemini_proxy_translates_eleven_segments_with_context() {
        let base = Url::parse(
            &std::env::var("KOHARU_COMPATIBLE_SMOKE_URL").expect("set the local proxy base URL"),
        )
        .unwrap();
        assert!(matches!(
            base.host_str(),
            Some("localhost" | "127.0.0.1" | "::1")
        ));
        let model =
            std::env::var("KOHARU_COMPATIBLE_SMOKE_MODEL").expect("set the Gemini model ID");
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(90))
            .build()
            .unwrap();
        let request = TranslationRequest::new(
            [
                "Wait for me!",
                "Why are there so many rocks here?",
                "Splash!",
                "Splash, splash!",
                "Ha ha ha!",
                "We should keep an eye on him.",
                "Wear a life jacket if you swim farther out.",
                "Kaoru!",
                "Hey, look over here!",
                "Don't go near the dangerous waves.",
                "Okay!",
            ],
            crate::Language::Vietnamese,
        )
        .with_context([crate::TranslationContext::new(
            "Let's go to the beach.",
            "Mình ra bãi biển đi.",
        )])
        .with_instructions("Use casual Vietnamese dialogue and concise sound effects.");
        let output = translate(
            &client,
            &OpenAiCompatibleConfig {
                base_url: Some(base),
            },
            &model,
            &GenerationConfig::default(),
            &request,
        )
        .await
        .unwrap();
        assert_eq!(output.len(), 11);
        assert!(output.iter().all(|text| !text.trim().is_empty()));
    }

    #[test]
    fn endpoint_preserves_base_path() {
        let url = Url::parse("http://localhost:1234/v1").unwrap();
        assert_eq!(
            endpoint(Some(&url), "models"),
            "http://localhost:1234/v1/models"
        );
    }

    #[test]
    fn serializes_compatible_request_contract() {
        let generation = GenerationConfig {
            max_tokens: Some(1024),
            ..GenerationConfig::default()
        };
        let request = TranslationRequest::new(["Hello", "World"], crate::Language::Vietnamese);
        let body = ChatRequest::new("model", &generation, &request).unwrap();
        let value = serde_json::to_value(body).unwrap();

        assert_eq!(value["max_tokens"], 1024);
        assert_eq!(value["stream"], false);
        assert_eq!(value["reasoning_effort"], "none");
        assert_eq!(value["response_format"]["type"], "json_schema");
        assert_eq!(value["response_format"]["json_schema"]["strict"], true);
        assert_eq!(
            value["response_format"]["json_schema"]["schema"]["properties"]["translations"]["maxItems"],
            2
        );
    }

    #[test]
    fn serializes_text_before_an_attached_image() {
        let request = TranslationRequest::new(["Hello"], crate::Language::Vietnamese)
            .with_image(std::sync::Arc::new(image::DynamicImage::new_rgb8(1, 1)));
        let body = ChatRequest::new(
            "ag/gemini-3.8-flash-high",
            &GenerationConfig::default(),
            &request,
        )
        .unwrap();
        let value = serde_json::to_value(body).unwrap();
        let parts = &value["messages"][1]["content"];
        assert_eq!(parts[0]["type"], "text");
        let input: serde_json::Value =
            serde_json::from_str(parts[0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(input["segments"][0]["text"], "Hello");
        assert_eq!(parts[1]["type"], "image_url");
        assert!(
            parts[1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/jpeg;base64,")
        );
    }
}
