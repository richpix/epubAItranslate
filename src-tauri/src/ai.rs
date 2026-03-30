use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::time::{sleep, Duration};

const MIN_CONTENT_CHARS_FOR_RATIO_CHECK: usize = 40;
const DEFAULT_DEEPSEEK_MODEL: &str = "deepseek-chat";
const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";
const DEEPSEEK_API_URL: &str = "https://api.deepseek.com/chat/completions";

/// Simple logging macro that prints to stderr (visible in Tauri dev console)
macro_rules! log_ai {
    ($($arg:tt)*) => {
        eprintln!("[EPUB-AI] {}", format!($($arg)*));
    };
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateApiKeyRequest {
    pub api_key: String,
    pub model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekChatCompletionResponse {
    choices: Vec<DeepSeekChoice>,
}

#[derive(Debug, Deserialize)]
struct DeepSeekChoice {
    message: DeepSeekChatMessage,
}

#[derive(Debug, Deserialize)]
struct DeepSeekChatMessage {
    content: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerateContentResponse {
    candidates: Vec<GeminiCandidate>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    text: Option<String>,
}

#[tauri::command]
pub async fn validate_api_key(request: ValidateApiKeyRequest) -> Result<bool, String> {
    if request.api_key.trim().is_empty() {
        return Err("La API key no puede estar vacia".to_string());
    }

    let model = normalized_model(request.model.as_deref());
    let provider = provider_for_model(model);

    let client = Client::new();
    let res = match provider {
        AiProvider::Gemini => {
            let validation_url = format!(
                "{}/{}:generateContent?key={}",
                GEMINI_API_BASE,
                model,
                request.api_key.trim()
            );

            client
                .post(validation_url)
                .header("Content-Type", "application/json")
                .json(&json!({
                    "contents": [
                        {
                            "role": "user",
                            "parts": [{ "text": "Reply with exactly: OK" }]
                        }
                    ],
                    "generationConfig": {
                        "temperature": 0
                    }
                }))
                .send()
                .await
        }
        AiProvider::DeepSeek => {
            client
                .post(DEEPSEEK_API_URL)
                .header("Authorization", format!("Bearer {}", request.api_key.trim()))
                .header("Content-Type", "application/json")
                .json(&json!({
                    "model": model,
                    "messages": [
                        {"role": "user", "content": "Reply with exactly: OK"}
                    ],
                    "max_tokens": 4,
                    "temperature": 0
                }))
                .send()
                .await
        }
    };

    match res {
        Ok(response) => {
            if response.status().is_success() {
                Ok(true)
            } else {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                Err(format!("Invalida / Error {} {} - {}", provider_label(provider), status, text))
            }
        }
        Err(e) => Err(format!("Error de red: {}", e)),
    }
}

pub async fn translate_text_with_retry(
    client: &Client,
    api_key: &str,
    model: Option<&str>,
    target_language: &str,
    text: &str,
    max_retries: u32,
) -> Result<String, String> {
    let mut attempt: u32 = 0;
    let mut delay_ms: u64 = 1_000;
    let sanitized_api_key = api_key.trim();
    let selected_model = normalized_model(model);
    let provider = provider_for_model(selected_model);

    log_ai!(
        "translate_text_with_retry: provider={}, model={}, lang={}, text_len={}, text_preview='{}'",
        provider_label(provider),
        selected_model,
        target_language,
        text.len(),
        preview_chars(text, 80)
    );

    loop {
        match translate_text_once(client, sanitized_api_key, selected_model, target_language, text).await {
            Ok(translated) => {
                log_ai!(
                    "Translation SUCCESS: '{}' -> '{}'",
                    preview_chars(text, 60),
                    preview_chars(&translated, 60)
                );
                return Ok(translated);
            }
            Err(err) => {
                log_ai!("Translation attempt {} FAILED: {}", attempt + 1, err);
                if attempt >= max_retries {
                    log_ai!("All {} retries exhausted, returning error", max_retries);
                    return Err(err);
                }

                sleep(Duration::from_millis(delay_ms)).await;
                attempt += 1;
                delay_ms = (delay_ms * 2).min(10_000);
            }
        }
    }
}

async fn translate_text_once(
    client: &Client,
    api_key: &str,
    model: &str,
    target_language: &str,
    text: &str,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }

    let translated = request_translation_completion(
        client,
        api_key,
        model,
        target_language,
        text,
        false,
    )
    .await?;

    validate_translation_output(text, &translated)?;

    if !seems_untranslated_for_target(text, &translated, target_language) {
        return Ok(translated);
    }

    log_ai!(
        "Detected likely untranslated output for target='{}'; retrying with strict prompt",
        target_language
    );

    let strict_translated = request_translation_completion(
        client,
        api_key,
        model,
        target_language,
        text,
        true,
    )
    .await?;

    validate_translation_output(text, &strict_translated)?;

    if seems_untranslated_for_target(text, &strict_translated, target_language) {
        // For debugging / resilience: even if the heuristic thinks the text is still in
        // the original language, do not fail the chunk (which would make us keep the source).
        log_ai!(
            "Untranslated-guard triggered, but returning strict translation anyway. provider={}, lang={}",
            provider_label(provider_for_model(model)),
            target_language
        );
        return Ok(strict_translated);
    }

    Ok(strict_translated)
}

async fn request_translation_completion(
    client: &Client,
    api_key: &str,
    model: &str,
    target_language: &str,
    text: &str,
    strict_mode: bool,
) -> Result<String, String> {
    let system_prompt = build_translation_system_prompt(target_language, strict_mode);

    match provider_for_model(model) {
        AiProvider::Gemini => {
            request_gemini_completion(client, api_key, model, &system_prompt, text, 0.1).await
        }
        AiProvider::DeepSeek => {
            request_deepseek_completion(client, api_key, model, &system_prompt, text, 0.1).await
        }
    }
}

fn build_translation_system_prompt(target_language: &str, strict_mode: bool) -> String {
    let base = format!(
        "Eres un traductor literario profesional. Traduce al {} con calidad editorial y naturalidad, como una traduccion humana cuidada. Reglas obligatorias: 1) No resumas ni omitas informacion. 2) No agregues explicaciones, notas ni encabezados. 3) Conserva el tono narrativo, estilo y matices. 4) Si aparecen etiquetas HTML/XML, no las modifiques ni las traduzcas. 5) Si aparecen entidades HTML (por ejemplo &amp;, &lt;, &gt;), conservaalas. 6) Respeta saltos de linea. Devuelve unicamente el texto traducido.",
        target_language
    );

    if strict_mode {
        format!(
            "{} REGLA ESTRICTA EXTRA: la salida debe quedar en {}. Si el texto fuente viene en otro idioma, no lo devuelvas igual; traducelo. Mantén solo nombres propios sin traducir.",
            base, target_language
        )
    } else {
        base
    }
}

fn validate_translation_output(source: &str, translated: &str) -> Result<(), String> {
    let source_trimmed = source.trim();
    let translated_trimmed = translated.trim();

    if translated_trimmed.is_empty() {
        return Err("El modelo IA devolvio una traduccion vacia".to_string());
    }

    if translated_trimmed.contains("```") {
        return Err("El modelo IA devolvio formato de bloque de codigo no permitido".to_string());
    }

    let source_has_tag_like = has_tag_like_fragment(source_trimmed);
    let translated_has_tag_like = has_tag_like_fragment(translated_trimmed);
    if !source_has_tag_like && translated_has_tag_like {
        return Err("El modelo IA introdujo etiquetas no esperadas".to_string());
    }

    let source_content_len = source_trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();
    let translated_content_len = translated_trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();

    // Only check for untranslated text on longer content
    if source_content_len >= MIN_CONTENT_CHARS_FOR_RATIO_CHECK {
        if translated_content_len * 4 < source_content_len {
            return Err("El modelo IA devolvio una traduccion demasiado corta".to_string());
        }

        if translated_content_len > source_content_len * 6 {
            return Err("El modelo IA devolvio una traduccion demasiado extensa".to_string());
        }
    }

    log_ai!("validate_translation_output PASSED: src_len={}, trans_len={}",
        source_content_len, translated_content_len);

    Ok(())
}

fn has_tag_like_fragment(input: &str) -> bool {
    let bytes = input.as_bytes();
    for idx in 0..bytes.len() {
        if bytes[idx] != b'<' {
            continue;
        }

        let next = bytes.get(idx + 1).copied();
        if matches!(next, Some(b'!') | Some(b'/')) {
            return true;
        }

        if let Some(value) = next {
            if value.is_ascii_alphabetic() {
                return true;
            }
        }
    }

    false
}

async fn request_gemini_completion(
    client: &Client,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_text: &str,
    temperature: f32,
) -> Result<String, String> {
    let endpoint = format!(
        "{}/{}:generateContent?key={}",
        GEMINI_API_BASE,
        model,
        api_key.trim()
    );

    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .json(&json!({
            "systemInstruction": {
                "parts": [{ "text": system_prompt }]
            },
            "contents": [
                {
                    "role": "user",
                    "parts": [{ "text": user_text }]
                }
            ],
            "generationConfig": {
                "temperature": temperature
            }
        }))
        .send()
        .await
        .map_err(|e| format!("Error de red: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Error de Gemini {}: {}", status, body));
    }

    let payload: GeminiGenerateContentResponse = response
        .json()
        .await
        .map_err(|e| format!("Respuesta invalida de Gemini: {}", e))?;

    let translated = payload
        .candidates
        .first()
        .and_then(|candidate| candidate.content.as_ref())
        .map(|content| {
            content
                .parts
                .iter()
                .filter_map(|part| part.text.as_deref())
                .collect::<String>()
                .trim()
                .to_string()
        })
        .filter(|content| !content.is_empty())
        .ok_or_else(|| "Gemini devolvio una respuesta vacia".to_string())?;

    Ok(translated)
}

async fn request_deepseek_completion(
    client: &Client,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    user_text: &str,
    temperature: f32,
) -> Result<String, String> {
    let response = client
        .post(DEEPSEEK_API_URL)
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": model,
            "temperature": temperature,
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt
                },
                {
                    "role": "user",
                    "content": user_text
                }
            ]
        }))
        .send()
        .await
        .map_err(|e| format!("Error de red: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Error de DeepSeek {}: {}", status, body));
    }

    let payload: DeepSeekChatCompletionResponse = response
        .json()
        .await
        .map_err(|e| format!("Respuesta invalida de DeepSeek: {}", e))?;

    let translated = payload
        .choices
        .first()
        .map(|choice| choice.message.content.trim().to_string())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| "DeepSeek devolvio una respuesta vacia".to_string())?;

    Ok(translated)
}

fn normalized_model(model: Option<&str>) -> &str {
    model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_DEEPSEEK_MODEL)
}

#[derive(Clone, Copy)]
enum AiProvider {
    DeepSeek,
    Gemini,
}

fn provider_for_model(model: &str) -> AiProvider {
    if model.to_ascii_lowercase().contains("gemini") {
        AiProvider::Gemini
    } else {
        AiProvider::DeepSeek
    }
}

fn provider_label(provider: AiProvider) -> &'static str {
    match provider {
        AiProvider::DeepSeek => "DeepSeek",
        AiProvider::Gemini => "Gemini",
    }
}

fn seems_untranslated_for_target(source: &str, translated: &str, target_language: &str) -> bool {
    let source_norm = normalize_alnum_lower(source);
    let translated_norm = normalize_alnum_lower(translated);

    if !source_norm.is_empty() && source_norm == translated_norm {
        return true;
    }

    let source_content_len = source
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();
    if source_content_len < MIN_CONTENT_CHARS_FOR_RATIO_CHECK {
        return false;
    }

    let target_is_spanish = target_language.to_ascii_lowercase().contains("espan");
    if target_is_spanish {
        let src_ratio = english_stopword_ratio(source);
        let trans_ratio = english_stopword_ratio(translated);
        // A solid ES translation usually drives English stopword density well below the source.
        if trans_ratio < 0.028 {
            return false;
        }
        const MIN_SRC_EN_RATIO: f64 = 0.055;
        if src_ratio >= MIN_SRC_EN_RATIO && trans_ratio >= src_ratio * 0.88 {
            log_ai!(
                "Likely untranslated EN->ES chunk (stopword ratios src={:.3} trans={:.3}): src='{}' trans='{}'",
                src_ratio,
                trans_ratio,
                preview_chars(source, 80),
                preview_chars(translated, 80)
            );
            return true;
        }
    }

    false
}

fn normalize_alnum_lower(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn preview_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

const ENGLISH_STOPWORDS: &[&str] = &[
    "the", "and", "of", "to", "in", "is", "that", "for", "with", "on", "as", "was", "at",
    "by", "from", "an", "be", "it", "or", "not", "have", "had", "has", "were", "been",
    "their", "they", "this", "which", "will", "would", "there", "could",
];

/// Share of ASCII word tokens that match common English function words.
fn english_stopword_ratio(input: &str) -> f64 {
    let lower = input.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|ch: char| !ch.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .collect();
    if words.is_empty() {
        return 0.0;
    }
    let hits = words
        .iter()
        .filter(|w| ENGLISH_STOPWORDS.contains(w))
        .count();
    hits as f64 / words.len() as f64
}
