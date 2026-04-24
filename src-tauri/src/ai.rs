use std::collections::HashMap;
use std::env;
use std::sync::{Mutex, OnceLock};

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::json;
use tokio::time::{sleep, Duration};

// Constantes de configuración para la validación de traducciones y límites de tokens de salida.
const MIN_CONTENT_CHARS_FOR_RATIO_CHECK: usize = 40;
const MAX_OUTPUT_TOKENS_ENV: &str = "EPUBTR_MAX_OUTPUT_TOKENS";
const MIN_OUTPUT_TOKENS: usize = 512;
const DEFAULT_OUTPUT_TOKENS_CAP: usize = 8_192;
const MAX_OUTPUT_TOKENS_CAP: usize = 12_288;
const GEMINI_MODEL: &str = "gemini-3.1-flash-lite-preview";
const OPENAI_MODEL: &str = "gpt-5.4-nano";
const DEEPSEEK_DEFAULT_MODEL: &str = "deepseek-v4-pro";
const DEEPSEEK_ALT_MODEL: &str = "deepseek-v4-flash";
// Caché estática en memoria para almacenar los prompts de sistema (system prompt) por idioma.
static SYSTEM_PROMPT_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

// Valida la clave de la API mediante una petición de prueba mínima al endpoint.
// Retorna Ok(true) si el servidor responde con éxito.
#[tauri::command]
pub async fn validate_api_key(api_key: String, provider: String, model: String) -> Result<bool, String> {
    if api_key.trim().is_empty() {
        return Err("La API key no puede estar vacia".to_string());
    }

    let provider_norm = normalize_provider(provider.as_str());
    let model_norm = normalize_model(provider_norm.as_str(), model.as_str());

    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| format!("No se pudo inicializar cliente HTTP: {}", e))?;
    let res = if provider_norm == "gemini" {
        let endpoint = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model_norm,
            api_key.trim()
        );
        client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .json(&json!({
                "contents": [{
                    "parts": [{"text": "Hello"}]
                }]
            }))
            .send()
            .await
    } else {
        let endpoint = match provider_norm.as_str() {
            "openai" => "https://api.openai.com/v1/chat/completions",
            _ => "https://api.deepseek.com/chat/completions",
        };
        client
            .post(endpoint)
            .header("Authorization", format!("Bearer {}", api_key.trim()))
            .header("Content-Type", "application/json")
            .header("Accept-Encoding", "identity")
            .json(&json!({
                "model": model_norm,
                "messages": [
                    {"role": "user", "content": "Hello"}
                ],
                "max_tokens": 1
            }))
            .send()
            .await
    };

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

// Envía texto a DeepSeek aplicando una estrategia de reintentos exponencial (exponential backoff).
// También posee un mecanismo de seguridad estricta para evitar la intrusión de caracteres chinos (han),
// re-evaluando con un prompt reforzado (fallback stricto).
pub async fn translate_text_with_retry(
    client: &Client,
    api_key: &str,
    provider: &str,
    model: &str,
    target_language: &str,
    text: &str,
    max_retries: u32,
) -> Result<String, String> {
    let mut attempt: u32 = 0;
    let mut delay_ms: u64 = 1_000;
    let sanitized_api_key = api_key.trim();

    loop {
        match translate_text_once(client, sanitized_api_key, provider, model, target_language, text).await {
            Ok(translated) => {
                if should_retry_for_han(target_language, &translated) {
                    match translate_text_once_strict_spanish(
                        client,
                        sanitized_api_key,
                        provider,
                        model,
                        target_language,
                        text,
                    )
                    .await
                    {
                        Ok(strict_translated) => {
                            if should_retry_for_han(target_language, &strict_translated) {
                                return Ok(translated);
                            }
                            return Ok(strict_translated);
                        }
                        Err(_) => return Ok(translated),
                    }
                }

                return Ok(translated);
            }
            Err(err) => {
                if is_non_retryable_error(&err) {
                    return Err(err);
                }

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

// Envía texto a DeepSeek a través de SSE (Server-Sent Events) permitiendo recibir la traducción en stream (por chunks).
// En caso de fallas de red, implementa un "exponential backoff" (aumentando la espera) hasta max_retries.
// Si no hay el callback 'on_delta', retrocede silenciosamente al flujo asíncrono normal.
pub async fn translate_text_with_retry_streaming(
    client: &Client,
    api_key: &str,
    provider: &str,
    model: &str,
    target_language: &str,
    text: &str,
    max_retries: u32,
    on_delta: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<String, String> {
    let mut attempt: u32 = 0;
    let mut delay_ms: u64 = 1_000;
    let sanitized_api_key = api_key.trim();

    if let Some(on_delta) = on_delta {
        loop {
            match translate_text_streaming_once(
                client,
                sanitized_api_key,
                provider,
                model,
                target_language,
                text,
                Some(on_delta),
            )
            .await
            {
                Ok(translated) => return Ok(translated),
                Err(err) => {
                    if is_non_retryable_error(&err) {
                        return Err(err);
                    }

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

    loop {
        match translate_text_streaming_once(
            client,
            sanitized_api_key,
            provider,
            model,
            target_language,
            text,
            None,
        )
        .await
        {
            Ok(translated) => return Ok(translated),
            Err(err) => {
                if is_non_retryable_error(&err) {
                    return Err(err);
                }

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

// Emite una única petición REST al endpoint de completamiento de DeepSeek.
// Calcula los tokens dinámicamente y se encarga de empaquetar una respuesta consolidada.
// Falla de entrada si el texto base está vacío.
async fn translate_text_once(
    client: &Client,
    api_key: &str,
    provider: &str,
    model: &str,
    target_language: &str,
    text: &str,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }

    let provider_norm = normalize_provider(provider);
    let model_norm = normalize_model(provider_norm.as_str(), model);
    let system_prompt = get_system_prompt(target_language);
    let max_tokens = resolve_max_output_tokens(text);

    if provider_norm == "gemini" {
        let translated = translate_gemini_once(
            client,
            api_key,
            model_norm.as_str(),
            system_prompt.as_str(),
            text,
        )
        .await?;
        validate_translation_output(text, &translated)?;
        return Ok(translated);
    }

    let endpoint = match provider_norm.as_str() {
        "openai" => "https://api.openai.com/v1/chat/completions",
        _ => "https://api.deepseek.com/chat/completions",
    };

    let response = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .header("Accept-Encoding", "identity")
        .json(&json!({
            "model": model_norm,
            "temperature": 0.1,
            "max_tokens": max_tokens,
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt
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
        return Err(format!("Error del proveedor {} ({}): {}", provider_norm, status, body));
    }

    let (translated, finish_reason) = extract_text_and_finish_reason_from_response(response).await?;

    if matches!(finish_reason.as_deref(), Some("length")) {
        return Err("TRUNCATED_BY_LENGTH".to_string());
    }

    validate_translation_output(text, &translated)?;
    Ok(translated)
}

// Envía una única petición de streaming activando "stream": true en DeepSeek.
// Construye un buffer re-ensamblando cada "[DONE]" o "data: ..." con sus deltas de payload
// Llamando a su vez el callback de actualización visual (on_delta).
async fn translate_text_streaming_once(
    client: &Client,
    api_key: &str,
    provider: &str,
    model: &str,
    target_language: &str,
    text: &str,
    mut on_delta: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }

    let provider_norm = normalize_provider(provider);
    let model_norm = normalize_model(provider_norm.as_str(), model);
    let system_prompt = get_system_prompt(target_language);
    let max_tokens = resolve_max_output_tokens(text);

    if provider_norm == "gemini" {
        let translated = translate_gemini_once(
            client,
            api_key,
            model_norm.as_str(),
            system_prompt.as_str(),
            text,
        )
        .await?;
        if let Some(callback) = on_delta.as_deref_mut() {
            callback(translated.as_str());
        }
        validate_translation_output(text, &translated)?;
        return Ok(translated);
    }

    let endpoint = match provider_norm.as_str() {
        "openai" => "https://api.openai.com/v1/chat/completions",
        _ => "https://api.deepseek.com/chat/completions",
    };

    let response = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .header("Accept-Encoding", "identity")
        .json(&json!({
            "model": model_norm,
            "temperature": 0.1,
            "max_tokens": max_tokens,
            "stream": true,
            "messages": [
                {
                    "role": "system",
                    "content": system_prompt
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
        return Err(format!("Error del proveedor {} ({}): {}", provider_norm, status, body));
    }

    let mut buffer = String::new();
    let mut translated = String::new();
    let mut finish_reason: Option<String> = None;
    let mut stream = response.bytes_stream();

    while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e: reqwest::Error| format!("Error leyendo stream: {}", e))?;
        let chunk_text = String::from_utf8_lossy(&bytes);
        buffer.push_str(&chunk_text);

        while let Some(event_end) = buffer.find("\n\n") {
            let event = buffer[..event_end].to_string();
            buffer = buffer[event_end + 2..].to_string();

            for line in event.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };

                let payload = data.trim();
                if payload == "[DONE]" {
                    break;
                }

                let json_value: serde_json::Value =
                    serde_json::from_str(payload).map_err(|e| {
                        format!("Respuesta streaming invalida del proveedor {}: {}", provider_norm, e)
                    })?;

                if let Some(reason) = json_value
                    .get("choices")
                    .and_then(|choices| choices.get(0))
                    .and_then(|choice| choice.get("finish_reason"))
                    .and_then(|value| value.as_str())
                {
                    finish_reason = Some(reason.to_string());
                }

                let delta = json_value
                    .get("choices")
                    .and_then(|choices| choices.get(0))
                    .and_then(|choice| {
                        choice
                            .get("delta")
                            .and_then(|delta| delta.get("content"))
                            .or_else(|| {
                                choice
                                    .get("message")
                                    .and_then(|message| message.get("content"))
                            })
                            .or_else(|| choice.get("text"))
                    })
                    .and_then(|value| value.as_str());

                if let Some(content) = delta {
                    if !content.is_empty() {
                        translated.push_str(content);
                        if let Some(callback) = on_delta.as_deref_mut() {
                            callback(content);
                        }
                    }
                }
            }
        }
    }

    let translated = translated.trim().to_string();
    if translated.is_empty() {
        return Err("DeepSeek devolvio una traduccion vacia".to_string());
    }

    if matches!(finish_reason.as_deref(), Some("length")) {
        return Err("TRUNCATED_BY_LENGTH".to_string());
    }

    validate_translation_output(text, &translated)?;
    Ok(translated)
}

// Analiza la heurística del texto resultante frente a la fuente para detectar alteraciones graves
// como respuestas vacías, envoltorios de código ("```"), inserción de etiquetas indeseadas
// o relaciones desproporcionadas en el tamaño del texto.
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

    if source_has_tag_like {
        let source_tag_markers = source_trimmed.matches('<').count();
        let translated_tag_markers = translated_trimmed.matches('<').count();
        if source_tag_markers >= 4 && translated_tag_markers * 2 < source_tag_markers {
            return Err(
                "DeepSeek devolvio una traduccion posiblemente incompleta (faltan etiquetas)"
                    .to_string(),
            );
        }
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
        if translated_content_len * 3 < source_content_len {
            return Err("DeepSeek devolvio una traduccion demasiado corta".to_string());
        }

        if translated_content_len > source_content_len * 6 {
            return Err("DeepSeek devolvio una traduccion demasiado extensa".to_string());
        }
    }

    Ok(())
}

// Analiza linealmente una cadena para identificar estructuras similares a fragmentos XML (<.../>)
// sin requerir un parser html completo.
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

// Retorna un prompt estándar o extraído del caché interno si ya existe para este idioma.
// Dicta explícitamente el rol, tono narrativo y tratamiento de formato HTML al modelo.
fn get_system_prompt(target_language: &str) -> String {
    let cache = SYSTEM_PROMPT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache_guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    if let Some(prompt) = cache_guard.get(target_language) {
        return prompt.clone();
    }

    let prompt = format!(
        "Eres un traductor literario profesional. Traduce al {} con calidad editorial y naturalidad, como una traduccion humana cuidada. Reglas obligatorias: 1) No resumas ni omitas informacion. 2) No agregues explicaciones, notas ni encabezados. 3) Conserva el tono narrativo, estilo y matices. 4) Si aparecen etiquetas HTML/XML, no las modifiques ni las traduzcas. 5) Si aparecen entidades HTML (por ejemplo &amp;, &lt;, &gt;), conservaalas. 6) Respeta saltos de linea. Devuelve unicamente el texto traducido.",
        target_language
    );

    cache_guard.insert(target_language.to_string(), prompt.clone());
    prompt
}

// Fallback estricto. Construye una ordenación más enérgica e incuestionable
// para evitar caracteres orientales (han), de utilidad en contextos en que
// la IA intercala ideogramas si desconoce un término local.
async fn translate_text_once_strict_spanish(
    client: &Client,
    api_key: &str,
    provider: &str,
    model: &str,
    target_language: &str,
    text: &str,
) -> Result<String, String> {
    if text.trim().is_empty() {
        return Ok(text.to_string());
    }

    let strict_prompt = format!(
        "{} IMPORTANTE: Responde SOLO en {}. NO uses chino simplificado ni tradicional. Si no sabes un termino, transliteralo o dejalo en el idioma original, pero nunca chino.",
        get_system_prompt(target_language),
        target_language
    );

    let provider_norm = normalize_provider(provider);
    let model_norm = normalize_model(provider_norm.as_str(), model);

    if provider_norm == "gemini" {
        let translated = translate_gemini_once(client, api_key, model_norm.as_str(), strict_prompt.as_str(), text).await?;
        validate_translation_output(text, &translated)?;
        return Ok(translated);
    }

    let max_tokens = resolve_max_output_tokens(text);

    let endpoint = match provider_norm.as_str() {
        "openai" => "https://api.openai.com/v1/chat/completions",
        _ => "https://api.deepseek.com/chat/completions",
    };

    let response = client
        .post(endpoint)
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .header("Accept-Encoding", "identity")
        .json(&json!({
            "model": model_norm,
            "temperature": 0.1,
            "max_tokens": max_tokens,
            "messages": [
                {
                    "role": "system",
                    "content": strict_prompt
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
        return Err(format!("Error de {} ({}): {}", provider_norm, status, body));
    }

    let (translated, finish_reason) = extract_text_and_finish_reason_from_response(response).await?;

    if matches!(finish_reason.as_deref(), Some("length")) {
        return Err("TRUNCATED_BY_LENGTH".to_string());
    }

    validate_translation_output(text, &translated)?;
    Ok(translated)
}

// Intercepta e interpreta el body de la petición DeepSeek.
// Extrae de forma segura el texto resultante así como la razón de finalización (finish_reason).
async fn extract_text_and_finish_reason_from_response(
    response: reqwest::Response,
) -> Result<(String, Option<String>), String> {
    let body_bytes = response
        .bytes()
        .await
        .map_err(|e| format!("DEEPSEEK_RESPONSE_BODY_DECODE_ERROR: {}", e))?;

    let payload: serde_json::Value = serde_json::from_slice(&body_bytes).map_err(|e| {
        let preview = safe_body_preview(&body_bytes, 280);
        format!(
            "Respuesta invalida de DeepSeek: {} (preview={})",
            e, preview
        )
    })?;

    let first_choice = payload
        .get("choices")
        .and_then(|choices| choices.as_array())
        .and_then(|choices| choices.first())
        .ok_or_else(|| "DeepSeek devolvio una respuesta sin opciones".to_string())?;

    let finish_reason = first_choice
        .get("finish_reason")
        .and_then(|value| value.as_str())
        .map(|value| value.to_string());

    let translated = extract_text_from_choice(first_choice)
        .ok_or_else(|| "DeepSeek devolvio una traduccion vacia".to_string())?;

    Ok((translated, finish_reason))
}

// Analiza recursivamente diferentes formatos y estructuras de "choices" generados
// por los Endpoints OpenAI compatibles para reunir sus fragmentos en texto crudo.
fn extract_text_from_choice(choice: &serde_json::Value) -> Option<String> {
    let as_string = |value: Option<&serde_json::Value>| {
        value
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };

    if let Some(text) = as_string(choice.get("text")) {
        return Some(text);
    }

    let message_content = choice
        .get("message")
        .and_then(|message| message.get("content"));

    if let Some(text) = as_string(message_content) {
        return Some(text);
    }

    let content_array = message_content.and_then(|value| value.as_array())?;
    let mut joined = String::new();

    for item in content_array {
        if let Some(text_piece) = item
            .get("text")
            .and_then(|value| value.as_str())
            .or_else(|| item.as_str())
        {
            joined.push_str(text_piece);
        }
    }

    let trimmed = joined.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

// Genera un extracto de logs seguro con caracteres tomados mediante iterador "char"
// evitando panics por partir el offset de bytes en el contorno de un carácter utf-8.
fn safe_body_preview(bytes: &[u8], max_chars: usize) -> String {
    String::from_utf8_lossy(bytes)
        .chars()
        .take(max_chars)
        .collect::<String>()
        .replace('\n', "\\n")
}

fn normalize_provider(provider: &str) -> String {
    match provider.trim().to_ascii_lowercase().as_str() {
        "gemini" => "gemini".to_string(),
        "openai" => "openai".to_string(),
        _ => "deepseek".to_string(),
    }
}

fn normalize_model(provider: &str, requested_model: &str) -> String {
    match provider {
        "gemini" => GEMINI_MODEL.to_string(),
        "openai" => OPENAI_MODEL.to_string(),
        _ => {
            let model = requested_model.trim();
            if model == DEEPSEEK_DEFAULT_MODEL || model == DEEPSEEK_ALT_MODEL {
                model.to_string()
            } else {
                DEEPSEEK_DEFAULT_MODEL.to_string()
            }
        }
    }
}

async fn translate_gemini_once(
    client: &Client,
    api_key: &str,
    model: &str,
    system_prompt: &str,
    text: &str,
) -> Result<String, String> {
    let endpoint = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
        model, api_key
    );

    let prompt = format!("{}\n\nTexto:\n{}", system_prompt, text);

    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .json(&json!({
            "contents": [{
                "parts": [{ "text": prompt }]
            }]
        }))
        .send()
        .await
        .map_err(|e| format!("Error de red: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(format!("Error de Gemini ({}): {}", status, body));
    }

    let payload: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Respuesta invalida de Gemini: {}", e))?;

    let mut output = String::new();
    if let Some(parts) = payload
        .get("candidates")
        .and_then(|candidates| candidates.get(0))
        .and_then(|candidate| candidate.get("content"))
        .and_then(|content| content.get("parts"))
        .and_then(|parts| parts.as_array())
    {
        for part in parts {
            if let Some(text_piece) = part.get("text").and_then(|value| value.as_str()) {
                output.push_str(text_piece);
            }
        }
    }

    let trimmed = output.trim().to_string();
    if trimmed.is_empty() {
        return Err("Gemini devolvio una traduccion vacia".to_string());
    }

    Ok(trimmed)
}

// Condición de retraducción estricta: Analiza si existe indeseadamente
// caracteres Han (chinos) en traducciones que apuntan a español.
fn should_retry_for_han(target_language: &str, translated: &str) -> bool {
    if !is_spanish_target(target_language) {
        return false;
    }

    let han_count = translated
        .chars()
        .filter(|ch| is_cjk_han(*ch))
        .count();

    han_count >= 3
}

// Verifica si la meta de origen de destino (en el prompt) declara explícitamente español.
fn is_spanish_target(target_language: &str) -> bool {
    let normalized = target_language.to_ascii_lowercase();
    normalized.contains("espan") || normalized == "es"
}

// Devuelve verdadero si un carácter dado entra dentro del bloque del código Unicode extendido y unificado CJK (Ideogramas Orientales).
fn is_cjk_han(ch: char) -> bool {
    matches!(
        ch as u32,
        0x4E00..=0x9FFF
            | 0x3400..=0x4DBF
            | 0x20000..=0x2A6DF
            | 0x2A700..=0x2B73F
            | 0x2B740..=0x2B81F
            | 0x2B820..=0x2CEAF
            | 0xF900..=0xFAFF
            | 0x2F800..=0x2FA1F
    )
}

// Analiza descriptores de la clase de error para discernir aquellos irrecuperables
// de aquellos red-asociados que sí ganarían provecho al reintentarse.
// Evita bucles colmando la red cuando la entrada de por sí detona truncation, un json envenenado...
fn is_non_retryable_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("truncated_by_length")
        || lower.contains("deepseek_response_body_decode_error")
        || lower.contains("respuesta invalida de deepseek")
}

// Determina dinámicamente cuántos max_tokens se deben declarar usando de métrica la densidad
// original del extracto en número de caracteres, más un abanico holgado y seguro.
fn resolve_max_output_tokens(text: &str) -> usize {
    if let Some(override_value) = read_env_usize(
        MAX_OUTPUT_TOKENS_ENV,
        MIN_OUTPUT_TOKENS,
        MAX_OUTPUT_TOKENS_CAP,
    ) {
        return override_value;
    }

    let chars = text.chars().count();
    let estimated = (chars * 7) / 20 + 256;
    estimated.clamp(MIN_OUTPUT_TOKENS, DEFAULT_OUTPUT_TOKENS_CAP)
}

// Parsea numéricamente de forma segura el valor dentro de una clave del entorno global de rust.
// Garantiza que la salida encaje dentro del intervalo configurado.
fn read_env_usize(key: &str, min: usize, max: usize) -> Option<usize> {
    let raw = env::var(key).ok()?;
    let parsed = raw.trim().parse::<usize>().ok()?;
    Some(parsed.clamp(min, max))
}
