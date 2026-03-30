use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use tokio::time::{sleep, Duration};

const MIN_CONTENT_CHARS_FOR_RATIO_CHECK: usize = 40;

#[derive(Debug, Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ChatMessage,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    content: String,
}

#[tauri::command]
pub async fn validate_api_key(api_key: String) -> Result<bool, String> {
    if api_key.trim().is_empty() {
        return Err("La API key no puede estar vacia".to_string());
    }

    let client = Client::new();
    let res = client
        .post("https://api.deepseek.com/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": "deepseek-chat",
            "messages": [
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1
        }))
        .send()
        .await;

    match res {
        Ok(response) => {
            if response.status().is_success() {
                Ok(true)
            } else {
                let status = response.status();
                let text = response.text().await.unwrap_or_default();
                Err(format!("Inválida / Error: {} - {}", status, text))
            }
        }
        Err(e) => Err(format!("Error de red: {}", e)),
    }
}

pub async fn translate_text_with_retry(
    client: &Client,
    api_key: &str,
    target_language: &str,
    text: &str,
    max_retries: u32,
) -> Result<String, String> {
    let mut attempt: u32 = 0;
    let mut delay_ms: u64 = 1_000;
    let sanitized_api_key = api_key.trim();

    loop {
        match translate_text_once(client, sanitized_api_key, target_language, text).await {
            Ok(translated) => return Ok(translated),
            Err(err) => {
                if attempt >= max_retries {
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
    target_language: &str,
    text: &str,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }

    let response = client
        .post("https://api.deepseek.com/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": "deepseek-chat",
            "temperature": 0.1,
            "messages": [
                {
                    "role": "system",
                    "content": format!(
                        "Eres un traductor literario profesional. Traduce al {} con calidad editorial y naturalidad, como una traduccion humana cuidada. Reglas obligatorias: 1) No resumas ni omitas informacion. 2) No agregues explicaciones, notas ni encabezados. 3) Conserva el tono narrativo, estilo y matices. 4) Si aparecen etiquetas HTML/XML, no las modifiques ni las traduzcas. 5) Si aparecen entidades HTML (por ejemplo &amp;, &lt;, &gt;), conservaalas. 6) Respeta saltos de linea. Devuelve unicamente el texto traducido.",
                        target_language
                    )
                },
                {
                    "role": "user",
                    "content": text
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

    let payload: ChatCompletionResponse = response
        .json()
        .await
        .map_err(|e| format!("Respuesta invalida de DeepSeek: {}", e))?;

    let translated = payload
        .choices
        .first()
        .map(|choice| choice.message.content.trim().to_string())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| "DeepSeek devolvio una traduccion vacia".to_string())?;

    validate_translation_output(text, &translated)?;
    Ok(translated)
}

fn validate_translation_output(source: &str, translated: &str) -> Result<(), String> {
    let source_trimmed = source.trim();
    let translated_trimmed = translated.trim();

    if translated_trimmed.is_empty() {
        return Err("DeepSeek devolvio una traduccion vacia".to_string());
    }

    if translated_trimmed.contains("```") {
        return Err("DeepSeek devolvio formato de bloque de codigo no permitido".to_string());
    }

    let source_has_tag_like = has_tag_like_fragment(source_trimmed);
    let translated_has_tag_like = has_tag_like_fragment(translated_trimmed);
    if !source_has_tag_like && translated_has_tag_like {
        return Err("DeepSeek introdujo etiquetas no esperadas".to_string());
    }

    let source_content_len = source_trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();
    let translated_content_len = translated_trimmed
        .chars()
        .filter(|c| !c.is_whitespace())
        .count();

    if source_content_len >= MIN_CONTENT_CHARS_FOR_RATIO_CHECK {
        if translated_content_len * 4 < source_content_len {
            return Err("DeepSeek devolvio una traduccion demasiado corta".to_string());
        }

        if translated_content_len > source_content_len * 6 {
            return Err("DeepSeek devolvio una traduccion demasiado extensa".to_string());
        }
    }

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
