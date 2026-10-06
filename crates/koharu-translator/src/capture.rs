//! Opt-in capture of translation exchanges for debugging.
//!
//! With `KOHARU_CAPTURE_TRANSLATION` set, every translation request and response
//! is written to `~/.koharu/debug/translation/<unix-millis>-<provider>/`, and the
//! most recent one is mirrored to `latest/` so it can be inspected without
//! sorting directories. Inline images are extracted from the request body into
//! sibling `request-image-*.jpg` files, because the captured JSON alone cannot
//! answer whether the provider received the original page or a cleaned-up page.
//! Credentials are redacted and capture failures never fail a translation.

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::ImageReader;
use reqwest::RequestBuilder;
use serde_json::{Map, Value, json};
use url::Url;

use crate::TranslationRequest;

const ENVIRONMENT: &str = "KOHARU_CAPTURE_TRANSLATION";
const LATEST: &str = "latest";
const MAX_EXCHANGES: usize = 5;
const REDACTED: &str = "[REDACTED]";
const BINARY: &str = "<binary>";
const REQUEST_FILE: &str = "request.json";
const RESPONSE_FILE: &str = "response.json";
const SECRET_HEADERS: [&str; 8] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "x-goog-iam-authorization-token",
    "cookie",
    "x-auth-token",
];
const SECRET_QUERY: [&str; 8] = [
    "key",
    "api_key",
    "apikey",
    "access_token",
    "token",
    "secret",
    "signature",
    "password",
];

#[cfg(test)]
static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Whether translation exchanges are persisted to disk.
///
/// The variable is read per exchange so a test or a restarted process can turn
/// capturing on or off without rebuilding the translator.
#[must_use]
pub fn enabled() -> bool {
    std::env::var(ENVIRONMENT).is_ok_and(|value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "" | "0" | "off" | "no" | "false"
        )
    })
}

/// The directory holding captured exchanges: `~/.koharu/debug/translation`.
pub fn root() -> Result<PathBuf> {
    #[cfg(test)]
    if let Some(root) = ROOT.get() {
        return Ok(root.clone());
    }
    let path = koharu_config::path()?;
    let parent = path
        .parent()
        .context("configuration path has no parent directory")?;
    Ok(parent.join("debug").join("translation"))
}

#[cfg(test)]
pub(crate) fn set_root(root: PathBuf) {
    let _ = ROOT.set(root);
}

/// Captured exchanges, oldest first. The `latest` mirror is not an exchange.
pub fn exchanges() -> Result<Vec<PathBuf>> {
    exchange_directories(&root()?)
}

/// The most recent captured exchange.
pub fn latest() -> Result<Option<PathBuf>> {
    let latest = root()?.join(LATEST);
    Ok(latest.is_dir().then_some(latest))
}

/// Delete every captured exchange.
pub fn clear() -> Result<()> {
    let root = root()?;
    if root.is_dir() {
        fs::remove_dir_all(&root)
            .with_context(|| format!("failed to remove `{}`", root.display()))?;
    }
    Ok(())
}

/// A request that is about to be sent, held until its response is known.
pub(crate) struct Exchange {
    provider: &'static str,
    transport: &'static str,
    request: Value,
    images: Vec<Image>,
}

impl Exchange {
    /// Persist the request together with the response document.
    pub(crate) fn finish(self, response: Value) {
        if let Err(error) = persist(&self, response) {
            tracing::warn!(%error, provider = self.provider, "failed to capture translation exchange");
        }
    }
}

struct Image {
    name: String,
    media_type: String,
    bytes: Vec<u8>,
}

impl Image {
    fn new(index: usize, media_type: &str, bytes: Vec<u8>) -> Self {
        Self {
            name: format!("request-image-{index}.{}", extension(media_type)),
            media_type: media_type.to_owned(),
            bytes,
        }
    }

    fn describe(&self) -> Value {
        let (width, height) = dimensions(&self.bytes);
        json!({
            "file": self.name,
            "media_type": self.media_type,
            "bytes": self.bytes.len(),
            "width": width,
            "height": height,
        })
    }
}

/// Capture a hosted provider request without consuming it.
pub(crate) fn http(provider: &'static str, request: &RequestBuilder) -> Option<Exchange> {
    if !enabled() {
        return None;
    }
    let cloned = request.try_clone().or_else(|| {
        tracing::warn!(%provider, "translation request body is not clonable, skipping capture");
        None
    })?;
    let request = match cloned.build() {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(%provider, %error, "failed to build translation request for capture");
            return None;
        }
    };

    let mut images = Vec::new();
    let body = request.body().and_then(|body| body.as_bytes()).map(|bytes| {
        let mut value = body_value(bytes);
        extract_images(&mut value, &mut images);
        value
    });
    Some(Exchange {
        provider,
        transport: "http",
        request: json!({
            "method": request.method().as_str(),
            "url": redact_url(request.url()),
            "headers": headers(request.headers()),
            "body": body,
        }),
        images,
    })
}

/// Capture a local model invocation, whose prompt replaces a request body.
pub(crate) fn local(request: &TranslationRequest, rendered_prompt: &str) -> Option<Exchange> {
    if !enabled() {
        return None;
    }
    let mut images = Vec::new();
    if let Some(image) = request.image.as_deref() {
        match crate::backend::encode_image(image) {
            Ok(encoded) => match STANDARD.decode(&encoded.data) {
                Ok(bytes) => images.push(Image::new(0, "image/jpeg", bytes)),
                Err(error) => tracing::warn!(%error, "failed to decode local translation image"),
            },
            Err(error) => {
                tracing::warn!(%error, "failed to encode local translation image for capture");
            }
        }
    }
    Some(Exchange {
        provider: "local",
        transport: "local",
        request: json!({
            "prompt": rendered_prompt,
            "segments": request.segments,
            "source_language": request.source_language.map(|language| language.tag()),
            "target_language": request.target_language.tag(),
            "instructions": request.instructions,
            "context": request.context,
        }),
        images,
    })
}

/// Interpret a response body as JSON, keeping non-JSON bodies as text.
pub(crate) fn body_value(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or_else(|_| match std::str::from_utf8(bytes) {
        Ok(text) => Value::String(text.to_owned()),
        Err(_) => Value::String(format!("<{} non-utf8 bytes>", bytes.len())),
    })
}

fn persist(exchange: &Exchange, response: Value) -> Result<()> {
    let root = root()?;
    let directory = create_directory(&root, exchange)?;
    write_exchange(&directory, exchange)?;
    write_json(&directory.join(RESPONSE_FILE), &response)?;
    let latest = root.join(LATEST);
    if latest != directory {
        replace_directory(&directory, &latest)?;
    }
    prune(&root)?;
    tracing::info!(
        provider = exchange.provider,
        path = %directory.display(),
        "captured translation exchange",
    );
    Ok(())
}

fn create_directory(root: &Path, exchange: &Exchange) -> Result<PathBuf> {
    let stamp = now();
    for attempt in 0..u32::MAX {
        let suffix = if attempt == 0 {
            String::new()
        } else {
            format!("-{attempt}")
        };
        let directory = root.join(format!(
            "{stamp}-{provider}{suffix}",
            provider = exchange.provider
        ));
        match fs::create_dir_all(&directory) {
            Ok(()) => return Ok(directory),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to create `{}`", directory.display()));
            }
        }
    }
    Err(anyhow::anyhow!("failed to allocate a capture directory"))
}

fn write_exchange(directory: &Path, exchange: &Exchange) -> Result<()> {
    let mut images = Vec::with_capacity(exchange.images.len());
    for image in &exchange.images {
        let path = directory.join(&image.name);
        fs::write(&path, &image.bytes)
            .with_context(|| format!("failed to write `{}`", path.display()))?;
        images.push(image.describe());
    }
    let request = json!({
        "captured_at": now(),
        "provider": exchange.provider,
        "transport": exchange.transport,
        "request": exchange.request,
        "images": images,
    });
    write_json(&directory.join(REQUEST_FILE), &request)
}

fn replace_directory(source: &Path, target: &Path) -> Result<()> {
    if target.is_dir() {
        fs::remove_dir_all(target)
            .with_context(|| format!("failed to replace `{}`", target.display()))?;
    }
    fs::create_dir_all(target)
        .with_context(|| format!("failed to create `{}`", target.display()))?;
    for entry in entries(source)? {
        let name = entry.file_name().context("capture file has no name")?;
        fs::copy(&entry, target.join(&name))
            .with_context(|| format!("failed to copy `{}`", entry.display()))?;
    }
    Ok(())
}

fn prune(root: &Path) -> Result<()> {
    let directories = exchange_directories(root)?;
    for directory in directories
        .iter()
        .take(directories.len().saturating_sub(MAX_EXCHANGES))
    {
        let _ = fs::remove_dir_all(directory);
    }
    Ok(())
}

/// Captured exchanges, oldest first. The `latest` mirror is not an exchange.
fn exchange_directories(root: &Path) -> Result<Vec<PathBuf>> {
    let directories = entries(root)?
        .into_iter()
        .filter(|entry| {
            entry.is_dir()
                && entry.file_name().is_some_and(|name| {
                    name.to_str()
                        .is_some_and(|name| name.starts_with(|character: char| character.is_ascii_digit()))
                })
        })
        .collect::<Vec<_>>();
    Ok(directories)
}

fn entries(directory: &Path) -> Result<Vec<PathBuf>> {
    if !directory.is_dir() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(directory)
        .with_context(|| format!("failed to read `{}`", directory.display()))?;
    let mut paths = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut content = serde_json::to_vec_pretty(value)
        .with_context(|| format!("failed to encode `{}`", path.display()))?;
    content.push(b'\n');
    fs::write(path, content).with_context(|| format!("failed to write `{}`", path.display()))
}

/// Replace inline `data:` URLs with references to the extracted image files.
fn extract_images(value: &mut Value, images: &mut Vec<Image>) {
    match value {
        Value::String(text) => {
            let Some((media_type, payload)) = data_url(text) else {
                return;
            };
            let Ok(bytes) = STANDARD.decode(payload) else {
                return;
            };
            let index = images.len();
            images.push(Image::new(index, media_type, bytes));
            *text = format!("<captured image {}>", images[index].name);
        }
        Value::Array(items) => items.iter_mut().for_each(|item| extract_images(item, images)),
        Value::Object(fields) => fields
            .values_mut()
            .for_each(|value| extract_images(value, images)),
        _ => {}
    }
}

fn data_url(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("data:")?;
    let (media_type, payload) = rest.split_once(";base64,")?;
    Some((media_type, payload))
}

fn extension(media_type: &str) -> &'static str {
    match media_type.trim().trim_start_matches("image/") {
        "jpeg" | "jpg" => "jpg",
        "png" => "png",
        "webp" => "webp",
        "gif" => "gif",
        _ => "bin",
    }
}

fn dimensions(bytes: &[u8]) -> (Option<u32>, Option<u32>) {
    let Ok(reader) = ImageReader::new(Cursor::new(bytes)).with_guessed_format() else {
        return (None, None);
    };
    match reader.into_dimensions() {
        Ok((width, height)) => (Some(width), Some(height)),
        Err(_) => (None, None),
    }
}

fn headers(headers: &reqwest::header::HeaderMap) -> Value {
    let fields = headers
        .iter()
        .map(|(name, value)| {
            let value = if is_secret(name.as_str()) {
                REDACTED.to_owned()
            } else {
                match value.to_str() {
                    Ok(text) => text.to_owned(),
                    Err(_) => BINARY.to_owned(),
                }
            };
            (name.as_str().to_owned(), Value::String(value))
        })
        .collect::<Map<String, Value>>();
    Value::Object(fields)
}

fn redact_url(url: &Url) -> String {
    if url.query().is_none() {
        return url.to_string();
    }
    let mut url = url.clone();
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if is_secret_query(&key) {
                REDACTED.to_owned()
            } else {
                value.into_owned()
            };
            (key.into_owned(), value)
        })
        .collect::<Vec<_>>();
    url.set_query(None);
    url.query_pairs_mut().extend_pairs(pairs);
    url.to_string()
}

fn is_secret(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    SECRET_HEADERS.contains(&name.as_str())
}

fn is_secret_query(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SECRET_QUERY.contains(&key.as_str())
}

fn now() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// `set_root` and the environment are process-wide, so only one test may
    /// exercise the on-disk path.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn exchange(request: Value, images: Vec<Image>) -> Exchange {
        Exchange {
            provider: "test",
            transport: "http",
            request,
            images,
        }
    }

    #[test]
    fn redacts_secret_headers_and_query_parameters() {
        let mut map = reqwest::header::HeaderMap::new();
        map.insert("authorization", "Bearer secret".parse().unwrap());
        map.insert("content-type", "application/json".parse().unwrap());
        let headers = headers(&map);
        assert_eq!(headers["authorization"], REDACTED);
        assert_eq!(headers["content-type"], "application/json");

        let url = redact_url(&Url::parse("https://host/v1/models?key=abc&pageSize=1000").unwrap());
        assert_eq!(url, "https://host/v1/models?key=%5BREDACTED%5D&pageSize=1000");
    }

    #[test]
    fn extracts_inline_images_and_leaves_other_payloads_intact() {
        let mut body = json!({
            "messages": [
                { "content": [
                    { "type": "text", "text": "translate" },
                    { "image_url": { "url": format!("data:image/jpeg;base64,{}", STANDARD.encode([1u8, 2, 3])) } }
                ] }
            ]
        });
        let mut images = Vec::new();
        extract_images(&mut body, &mut images);

        assert_eq!(images.len(), 1);
        assert_eq!(images[0].name, "request-image-0.jpg");
        assert_eq!(images[0].bytes, vec![1, 2, 3]);
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "<captured image request-image-0.jpg>"
        );
        assert_eq!(body["messages"][0]["content"][0]["text"], "translate");
    }

    #[test]
    fn keeps_non_json_bodies_as_text() {
        assert_eq!(body_value(br#"{"ok":true}"#), json!({ "ok": true }));
        assert_eq!(
            body_value(b"text=hi&target_lang=ZH"),
            Value::String("text=hi&target_lang=ZH".to_owned())
        );
    }

    #[test]
    fn writes_an_exchange_and_mirrors_it_as_latest() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let exchange = exchange(
            json!({ "body": { "model": "test-model" } }),
            vec![Image::new(0, "image/jpeg", vec![7, 8, 9])],
        );

        let target = create_directory(root, &exchange).unwrap();
        write_exchange(&target, &exchange).unwrap();
        replace_directory(&target, &root.join(LATEST)).unwrap();

        let latest = root.join(LATEST);
        let request =
            serde_json::from_slice::<Value>(&fs::read(latest.join(REQUEST_FILE)).unwrap()).unwrap();
        assert_eq!(request["provider"], "test");
        assert_eq!(request["request"]["body"]["model"], "test-model");
        assert_eq!(request["images"][0]["file"], "request-image-0.jpg");
        assert_eq!(request["images"][0]["bytes"], 3);
        assert_eq!(
            fs::read(latest.join("request-image-0.jpg")).unwrap(),
            vec![7, 8, 9]
        );
    }

    #[tokio::test]
    async fn captures_a_live_request_and_its_response() {
        let _serial = SERIAL.lock().unwrap_or_else(|error| error.into_inner());
        let directory = tempfile::tempdir().unwrap();
        set_root(directory.path().join("translation"));
        // SAFETY: no other thread reads this variable while the guard is held.
        unsafe { std::env::set_var(ENVIRONMENT, "1") };
        assert!(enabled());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::task::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0; 4096];
            let read = stream.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).into_owned();
            let body = r#"{"choices":[]}"#;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            request
        });

        let body = json!({ "text": "こんにちは", "image": format!("data:image/jpeg;base64,{}", STANDARD.encode([4u8, 5, 6])) });
        let exchange = http(
            "openai",
            &reqwest::Client::new()
                .post(format!("http://{address}/v1/chat/completions?key=secret"))
                .bearer_auth("secret")
                .json(&body),
        )
        .expect("capturing is enabled");

        // Send the real request so the mock server observes the same payload.
        let sent = reqwest::Client::new()
            .post(format!("http://{address}/v1/chat/completions?key=secret"))
            .bearer_auth("secret")
            .json(&body)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let observed = server.await.unwrap();
        exchange.finish(json!({
            "status": 200,
            "success": true,
            "body": body_value(sent.as_bytes()),
        }));

        let latest = latest().unwrap().expect("latest mirror exists");
        let request =
            serde_json::from_slice::<Value>(&fs::read(latest.join(REQUEST_FILE)).unwrap()).unwrap();
        assert_eq!(request["provider"], "openai");
        assert_eq!(request["transport"], "http");
        assert_eq!(request["request"]["method"], "POST");
        assert_eq!(request["request"]["headers"]["authorization"], REDACTED);
        assert_eq!(request["request"]["url"], format!("http://{address}/v1/chat/completions?key=%5BREDACTED%5D"));
        assert_eq!(request["request"]["body"]["text"], "こんにちは");
        assert_eq!(
            request["request"]["body"]["image"],
            "<captured image request-image-0.jpg>"
        );
        assert_eq!(request["images"][0]["bytes"], 3);
        assert_eq!(
            fs::read(latest.join("request-image-0.jpg")).unwrap(),
            vec![4, 5, 6]
        );
        assert!(observed.contains("Bearer secret"), "the request reached the server unchanged");
        assert!(observed.contains("こんにちは"));

        let response =
            serde_json::from_slice::<Value>(&fs::read(latest.join(RESPONSE_FILE)).unwrap()).unwrap();
        assert_eq!(response["status"], 200);
        assert_eq!(response["body"]["choices"], json!([]));
    }
}
