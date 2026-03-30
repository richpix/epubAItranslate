use std::fs::File;
use std::io::{Read, Write};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::ai;

const APPROX_CHARS_PER_PAGE: usize = 1800;
const DEFAULT_PREVIEW_PAGES: usize = 5;
const MAX_RETRIES: u32 = 4;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateEpubRequest {
    pub input_path: String,
    pub output_path: String,
    pub target_language: String,
    pub api_key: String,
    pub preview_only: bool,
    pub preview_pages: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateEpubResult {
    pub output_path: String,
    pub total_html_files: usize,
    pub translated_html_files: usize,
    pub translated_characters: usize,
    pub preview_only: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TranslationProgressPayload {
    status: String,
    message: String,
    current_file: usize,
    total_files: usize,
    percent: f32,
    translated_characters: usize,
}

#[derive(Debug, Clone)]
enum HtmlTokenKind {
    Tag,
    Text,
}

#[derive(Debug, Clone)]
struct HtmlToken {
    kind: HtmlTokenKind,
    value: String,
}

#[tauri::command]
pub async fn translate_epub(
    app: tauri::AppHandle,
    request: TranslateEpubRequest,
) -> Result<TranslateEpubResult, String> {
    let input_path = request.input_path.trim();
    let output_path = request.output_path.trim();

    if input_path.is_empty() || output_path.is_empty() {
        return Err("Las rutas de entrada y salida son obligatorias".to_string());
    }

    if request.api_key.trim().is_empty() {
        return Err("La API key de DeepSeek es obligatoria".to_string());
    }

    if !request.preview_only {
        return Err("La traduccion de libro completo esta deshabilitada en este MVP".to_string());
    }

    if !input_path.to_lowercase().ends_with(".epub") {
        return Err("Solo se admiten archivos .epub".to_string());
    }

    let mut reader = ZipArchive::new(
        File::open(input_path).map_err(|e| format!("No se pudo abrir el EPUB: {}", e))?,
    )
    .map_err(|e| format!("EPUB invalido: {}", e))?;

    let total_files = reader.len();
    let html_entry_indexes = (0..total_files)
        .filter_map(|i| {
            reader
                .by_index(i)
                .ok()
                .and_then(|entry| is_html_path(entry.name()).then_some(i))
        })
        .collect::<Vec<_>>();

    let total_html_files = html_entry_indexes.len();

    emit_progress(
        &app,
        TranslationProgressPayload {
            status: "starting".to_string(),
            message: "Iniciando traduccion".to_string(),
            current_file: 0,
            total_files: total_html_files,
            percent: 0.0,
            translated_characters: 0,
        },
    );

    let output_file = File::create(output_path)
        .map_err(|e| format!("No se pudo crear el EPUB de salida: {}", e))?;
    let mut writer = ZipWriter::new(output_file);
    let client = Client::new();

    let preview_pages = request
        .preview_pages
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_PREVIEW_PAGES)
        .max(1);
    let preview_limit = preview_pages * APPROX_CHARS_PER_PAGE;

    let mut translated_html_files = 0usize;
    let mut translated_characters = 0usize;

    for index in 0..total_files {
        let (file_name, options, is_directory, bytes) = {
            let mut entry = reader
                .by_index(index)
                .map_err(|e| format!("No se pudo leer la entrada EPUB {index}: {e}"))?;

            let file_name = entry.name().to_string();
            let mut options = FileOptions::default().compression_method(entry.compression());
            if let Some(mode) = entry.unix_mode() {
                options = options.unix_permissions(mode);
            }

            if entry.is_dir() {
                (file_name, options, true, Vec::new())
            } else {
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|e| format!("No se pudieron leer bytes de entrada: {}", e))?;
                (file_name, options, false, bytes)
            }
        };

        if is_directory {
            writer
                .add_directory(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo escribir directorio en salida: {}", e))?;
            continue;
        }

        if is_html_path(&file_name) {
            let source_html = match String::from_utf8(bytes) {
                Ok(content) => content,
                Err(err) => {
                    let original_bytes = err.into_bytes();
                    writer
                        .start_file(file_name.as_str(), options)
                        .map_err(|e| format!("No se pudo crear entrada de archivo en salida: {}", e))?;
                    writer
                        .write_all(&original_bytes)
                        .map_err(|e| format!("No se pudo escribir entrada sin traducir: {}", e))?;
                    continue;
                }
            };

            let should_translate = !request.preview_only || translated_characters < preview_limit;
            let (translated_html, consumed_chars) = if should_translate {
                translate_html_content(
                    &client,
                    request.api_key.trim(),
                    language_label(&request.target_language),
                    &source_html,
                    if request.preview_only {
                        Some(preview_limit.saturating_sub(translated_characters))
                    } else {
                        None
                    },
                )
                .await?
            } else {
                (source_html, 0)
            };

            translated_characters += consumed_chars;
            translated_html_files += 1;

            writer
                .start_file(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo crear entrada de archivo en salida: {}", e))?;
            writer
                .write_all(translated_html.as_bytes())
                .map_err(|e| format!("No se pudo escribir entrada traducida: {}", e))?;

            let current_html_index = translated_html_files;
            let percent = if total_html_files == 0 {
                100.0
            } else {
                (current_html_index as f32 / total_html_files as f32) * 100.0
            };

            emit_progress(
                &app,
                TranslationProgressPayload {
                    status: "processing".to_string(),
                    message: format!("Traduciendo {}", file_name),
                    current_file: current_html_index,
                    total_files: total_html_files,
                    percent,
                    translated_characters,
                },
            );
        } else {
            writer
                .start_file(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo crear entrada de copia directa: {}", e))?;
            writer
                .write_all(&bytes)
                .map_err(|e| format!("No se pudo escribir entrada de copia directa: {}", e))?;
        }
    }

    writer
        .finish()
        .map_err(|e| format!("No se pudo finalizar el EPUB de salida: {}", e))?;

    emit_progress(
        &app,
        TranslationProgressPayload {
            status: "completed".to_string(),
            message: "Traduccion finalizada".to_string(),
            current_file: translated_html_files,
            total_files: total_html_files,
            percent: 100.0,
            translated_characters,
        },
    );

    Ok(TranslateEpubResult {
        output_path: output_path.to_string(),
        total_html_files,
        translated_html_files,
        translated_characters,
        preview_only: request.preview_only,
    })
}

fn is_html_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")
}

fn language_label(code: &str) -> &str {
    match code {
        "es" => "espanol",
        "en" => "ingles",
        "fr" => "frances",
        "de" => "aleman",
        _ => "espanol",
    }
}

fn emit_progress(app: &tauri::AppHandle, payload: TranslationProgressPayload) {
    let _ = app.emit("translation-progress", payload);
}

async fn translate_html_content(
    client: &Client,
    api_key: &str,
    target_language: &str,
    html: &str,
    char_budget: Option<usize>,
) -> Result<(String, usize), String> {
    let tokens = tokenize_html(html);
    let mut result = String::with_capacity(html.len());

    let mut consumed_characters = 0usize;
    let mut skip_tag_depth = 0usize;
    let mut budget_exhausted = false;

    for token in tokens {
        match token.kind {
            HtmlTokenKind::Tag => {
                update_skip_depth(&token.value, &mut skip_tag_depth);
                result.push_str(&token.value);
            }
            HtmlTokenKind::Text => {
                if skip_tag_depth > 0 || token.value.trim().is_empty() || budget_exhausted {
                    result.push_str(&token.value);
                    continue;
                }

                let remaining = char_budget.map(|limit| limit.saturating_sub(consumed_characters));
                let (translated_text, consumed, exhausted_now) =
                    translate_text_preserving_whitespace(
                        client,
                        api_key,
                        target_language,
                        &token.value,
                        remaining,
                    )
                    .await?;

                consumed_characters += consumed;
                if exhausted_now {
                    budget_exhausted = true;
                }

                result.push_str(&translated_text);
            }
        }
    }

    Ok((result, consumed_characters))
}

async fn translate_text_preserving_whitespace(
    client: &Client,
    api_key: &str,
    target_language: &str,
    text: &str,
    remaining_chars: Option<usize>,
) -> Result<(String, usize, bool), String> {
    if text.trim().is_empty() {
        return Ok((text.to_string(), 0, false));
    }

    let trimmed = text.trim();
    let Some(start) = text.find(trimmed) else {
        return Ok((text.to_string(), 0, false));
    };
    let end = start + trimmed.len();

    let leading = &text[..start];
    let trailing = &text[end..];

    let (input_to_translate, consumed, exhausted_now) = if let Some(remaining) = remaining_chars {
        if remaining == 0 {
            return Ok((text.to_string(), 0, true));
        }

        let trimmed_chars = trimmed.chars().count();
        if trimmed_chars <= remaining {
            (trimmed.to_string(), trimmed_chars, false)
        } else {
            let (head, tail) = split_at_char_count(trimmed, remaining);
            if head.trim().is_empty() {
                return Ok((text.to_string(), 0, true));
            }

            let translated_head = ai::translate_text_with_retry(
                client,
                api_key,
                target_language,
                head,
                MAX_RETRIES,
            )
            .await?;

            return Ok((
                format!("{}{}{}{}", leading, translated_head, tail, trailing),
                remaining,
                true,
            ));
        }
    } else {
        (trimmed.to_string(), trimmed.chars().count(), false)
    };

    let translated = ai::translate_text_with_retry(
        client,
        api_key,
        target_language,
        &input_to_translate,
        MAX_RETRIES,
    )
    .await?;

    Ok((
        format!("{}{}{}", leading, translated, trailing),
        consumed,
        exhausted_now,
    ))
}

fn split_at_char_count(input: &str, char_count: usize) -> (&str, &str) {
    if char_count == 0 {
        return ("", input);
    }

    let split_index = input
        .char_indices()
        .nth(char_count)
        .map(|(idx, _)| idx)
        .unwrap_or(input.len());

    (&input[..split_index], &input[split_index..])
}

fn tokenize_html(html: &str) -> Vec<HtmlToken> {
    let mut tokens = Vec::new();

    let mut start = 0usize;
    let mut in_tag = false;

    for (idx, ch) in html.char_indices() {
        if ch == '<' && !in_tag {
            if start < idx {
                tokens.push(HtmlToken {
                    kind: HtmlTokenKind::Text,
                    value: html[start..idx].to_string(),
                });
            }
            in_tag = true;
            start = idx;
            continue;
        }

        if ch == '>' && in_tag {
            let end = idx + ch.len_utf8();
            tokens.push(HtmlToken {
                kind: HtmlTokenKind::Tag,
                value: html[start..end].to_string(),
            });
            in_tag = false;
            start = end;
        }
    }

    if start < html.len() {
        let trailing = &html[start..];
        tokens.push(HtmlToken {
            kind: if in_tag {
                HtmlTokenKind::Tag
            } else {
                HtmlTokenKind::Text
            },
            value: trailing.to_string(),
        });
    }

    tokens
}

fn update_skip_depth(tag: &str, depth: &mut usize) {
    let normalized = tag.trim();
    if normalized.starts_with("<!") || normalized.starts_with("<?") {
        return;
    }

    if let Some((name, is_closing, is_self_closing)) = parse_tag_name(normalized) {
        let lower = name.to_ascii_lowercase();
        if lower != "script" && lower != "style" {
            return;
        }

        if is_closing {
            if *depth > 0 {
                *depth -= 1;
            }
            return;
        }

        if !is_self_closing {
            *depth += 1;
        }
    }
}

fn parse_tag_name(tag: &str) -> Option<(&str, bool, bool)> {
    let bytes = tag.as_bytes();
    if bytes.len() < 3 || bytes.first().copied() != Some(b'<') {
        return None;
    }

    let mut idx = 1usize;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }

    let mut is_closing = false;
    if idx < bytes.len() && bytes[idx] == b'/' {
        is_closing = true;
        idx += 1;
    }

    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }

    let start = idx;
    while idx < bytes.len() {
        let ch = bytes[idx];
        if ch.is_ascii_alphanumeric() || ch == b':' || ch == b'-' || ch == b'_' {
            idx += 1;
        } else {
            break;
        }
    }

    if start == idx || idx > tag.len() {
        return None;
    }

    let is_self_closing = tag.trim_end().ends_with("/>");
    Some((&tag[start..idx], is_closing, is_self_closing))
}
