use std::fs::File;
use std::io::{Read, Write};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use zip::write::FileOptions;
use zip::{ZipArchive, ZipWriter};

use crate::ai;

/// Simple logging macro that prints to stderr (visible in Tauri dev console)
macro_rules! log_info {
    ($($arg:tt)*) => {
        eprintln!("[EPUB-TR] {}", format!($($arg)*));
    };
}

const APPROX_CHARS_PER_PAGE: usize = 1800;
const DEFAULT_PREVIEW_PAGES: usize = 5;
const MAX_RETRIES: u32 = 4;
const MIN_CONTENT_TEXT_CHARS: usize = 50;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateEpubRequest {
    pub input_path: String,
    pub output_path: String,
    pub target_language: String,
    pub api_key: String,
    pub model: Option<String>,
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

/// Represents a single entry read from the source EPUB, held in memory
struct EpubEntry {
    file_name: String,
    compression: zip::CompressionMethod,
    unix_mode: Option<u32>,
    is_directory: bool,
    bytes: Vec<u8>,
}

#[tauri::command]
pub async fn translate_epub(
    app: tauri::AppHandle,
    request: TranslateEpubRequest,
) -> Result<TranslateEpubResult, String> {
    let input_path = request.input_path.trim().to_string();
    let output_path = request.output_path.trim().to_string();

    if input_path.is_empty() || output_path.is_empty() {
        return Err("Las rutas de entrada y salida son obligatorias".to_string());
    }

    if request.api_key.trim().is_empty() {
        return Err("La API key del modelo IA es obligatoria".to_string());
    }

    // Full book translation is now allowed
    log_info!("preview_only={}, target_language={}", request.preview_only, request.target_language);

    if !input_path.to_lowercase().ends_with(".epub") {
        return Err("Solo se admiten archivos .epub".to_string());
    }

    let api_key = request.api_key.trim().to_string();
    let model = request.model.clone();
    let target_language = request.target_language.clone();
    let preview_only = request.preview_only;
    let preview_pages = request.preview_pages;

    // Run heavy work on a spawned task and await the join handle to preserve panic details.
    let task = tauri::async_runtime::spawn(async move {
        do_translate_epub(
            &app,
            &input_path,
            &output_path,
            &api_key,
            model.as_deref(),
            &target_language,
            preview_only,
            preview_pages,
        )
        .await
    });

    task.await
        .map_err(|e| format!("La tarea de traduccion finalizo inesperadamente: {}", e))?
}

async fn do_translate_epub(
    app: &tauri::AppHandle,
    input_path: &str,
    output_path: &str,
    api_key: &str,
    model: Option<&str>,
    target_language: &str,
    preview_only: bool,
    preview_pages: Option<u32>,
) -> Result<TranslateEpubResult, String> {
    // Read all entries from the source EPUB into memory first (blocking I/O)
    let (entries, total_files) = {
        let file = File::open(input_path)
            .map_err(|e| format!("No se pudo abrir el EPUB: {}", e))?;
        let mut reader = ZipArchive::new(file)
            .map_err(|e| format!("EPUB invalido: {}", e))?;

        let total = reader.len();
        let mut entries = Vec::with_capacity(total);

        for index in 0..total {
            let mut entry = reader
                .by_index(index)
                .map_err(|e| format!("No se pudo leer la entrada EPUB {index}: {e}"))?;

            let file_name = entry.name().to_string();
            let compression = entry.compression();
            let unix_mode = entry.unix_mode();

            if entry.is_dir() {
                entries.push(EpubEntry {
                    file_name,
                    compression,
                    unix_mode,
                    is_directory: true,
                    bytes: Vec::new(),
                });
            } else {
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|e| format!("No se pudieron leer bytes de entrada: {}", e))?;
                entries.push(EpubEntry {
                    file_name,
                    compression,
                    unix_mode,
                    is_directory: false,
                    bytes,
                });
            }
        }

        (entries, total)
    };

    let total_html_files = entries
        .iter()
        .filter(|e| !e.is_directory && is_html_path(&e.file_name))
        .count();

    emit_progress(
        app,
        TranslationProgressPayload {
            status: "starting".to_string(),
            message: "Iniciando traduccion".to_string(),
            current_file: 0,
            total_files: total_html_files,
            percent: 0.0,
            translated_characters: 0,
        },
    );

    let client = Client::new();

    let pages = preview_pages
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_PREVIEW_PAGES)
        .max(1);
    let preview_limit = pages * APPROX_CHARS_PER_PAGE;

    let mut translated_html_files = 0usize;
    let mut attempted_characters = 0usize;
    let mut translated_characters = 0usize;

    // Collect all output entries (translated or copied)
    let mut output_entries: Vec<(String, zip::CompressionMethod, Option<u32>, bool, Vec<u8>)> =
        Vec::with_capacity(total_files);

    for epub_entry in &entries {
        if epub_entry.is_directory {
            output_entries.push((
                epub_entry.file_name.clone(),
                epub_entry.compression,
                epub_entry.unix_mode,
                true,
                Vec::new(),
            ));
            continue;
        }

        if is_html_path(&epub_entry.file_name) {
            let source_html = decode_html_bytes(&epub_entry.bytes);

            let is_content_file = is_content_candidate(&epub_entry.file_name, &source_html);
            let should_translate = if preview_only {
                is_content_file && attempted_characters < preview_limit
            } else {
                is_content_file
            };

            log_info!(
                "File: {} | is_html=true | is_content={} | should_translate={} | attempted_so_far={} | translated_so_far={}",
                epub_entry.file_name,
                is_content_file,
                should_translate,
                attempted_characters,
                translated_characters
            );

            let (translated_html, attempted_in_file, translated_in_file) = if should_translate {
                translate_html_content(
                    &client,
                    api_key,
                    model,
                    language_label(target_language),
                    &source_html,
                    if preview_only {
                        Some(preview_limit.saturating_sub(attempted_characters))
                    } else {
                        None
                    },
                )
                .await
                .map_err(|e| format!("Fallo al traducir {}: {}", epub_entry.file_name, e))?
            } else {
                (source_html, 0, 0)
            };

            attempted_characters += attempted_in_file;
            translated_characters += translated_in_file;
            if translated_in_file > 0 {
                translated_html_files += 1;
            }

            log_info!(
                "Counters after file: attempted={} translated={} limit={}",
                attempted_characters,
                translated_characters,
                preview_limit
            );

            let current_html_index = translated_html_files;
            let percent = if total_html_files == 0 {
                100.0
            } else {
                (current_html_index as f32 / total_html_files as f32) * 100.0
            };

            emit_progress(
                app,
                TranslationProgressPayload {
                    status: "processing".to_string(),
                    message: format!("Traduciendo {}", epub_entry.file_name),
                    current_file: current_html_index,
                    total_files: total_html_files,
                    percent,
                    translated_characters,
                },
            );

            output_entries.push((
                epub_entry.file_name.clone(),
                epub_entry.compression,
                epub_entry.unix_mode,
                false,
                translated_html.into_bytes(),
            ));
        } else {
            output_entries.push((
                epub_entry.file_name.clone(),
                epub_entry.compression,
                epub_entry.unix_mode,
                false,
                epub_entry.bytes.clone(),
            ));
        }
    }

    // Write all output entries to the EPUB (blocking I/O, done at the end)
    let output_file = File::create(output_path)
        .map_err(|e| format!("No se pudo crear el EPUB de salida: {}", e))?;
    let mut writer = ZipWriter::new(output_file);

    for (file_name, compression, unix_mode, is_dir, bytes) in &output_entries {
        let mut options = FileOptions::default().compression_method(*compression);
        if let Some(mode) = unix_mode {
            options = options.unix_permissions(*mode);
        }

        if *is_dir {
            writer
                .add_directory(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo escribir directorio en salida: {}", e))?;
        } else {
            writer
                .start_file(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo crear entrada de archivo en salida: {}", e))?;
            writer
                .write_all(bytes)
                .map_err(|e| format!("No se pudo escribir entrada en salida: {}", e))?;
        }
    }

    writer
        .finish()
        .map_err(|e| format!("No se pudo finalizar el EPUB de salida: {}", e))?;

    if translated_characters == 0 {
        return Err(
            format!(
                "No se tradujo contenido visible del libro. Se intentaron {} caracteres. Revisa API key, limites de API o el formato del EPUB.",
                attempted_characters
            ),
        );
    }

    emit_progress(
        app,
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
        preview_only,
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

fn decode_html_bytes(bytes: &[u8]) -> String {
    match String::from_utf8(bytes.to_vec()) {
        Ok(content) => content,
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn epub_html_stem_lower(file_name: &str) -> String {
    let lower = file_name.to_lowercase();
    let base = lower.rsplit(['/', '\\']).next().unwrap_or(&lower);
    match base.rsplit_once('.') {
        Some((stem, ext)) => {
            let e = ext.to_ascii_lowercase();
            if e == "xhtml" || e == "html" || e == "htm" {
                stem.to_string()
            } else {
                base.to_string()
            }
        }
        None => base.to_string(),
    }
}

/// Skip typical EPUB package files (exact stem match), not arbitrary paths containing e.g. "nav".
fn is_boilerplate_epub_html_name(file_name: &str) -> bool {
    let stem = epub_html_stem_lower(file_name);
    matches!(
        stem.as_str(),
        "nav" | "toc" | "contents" | "cover" | "titlepage" | "copyright"
    )
}

fn is_content_candidate(file_name: &str, html: &str) -> bool {
    if is_boilerplate_epub_html_name(file_name) {
        log_info!("SKIP (boilerplate filename): {}", file_name);
        return false;
    }

    let plain_text_len = tokenize_html(html)
        .into_iter()
        .filter_map(|token| match token.kind {
            HtmlTokenKind::Text => Some(token.value),
            HtmlTokenKind::Tag => None,
        })
        .map(|text| {
            text.chars()
                .filter(|ch| ch.is_alphabetic())
                .count()
        })
        .sum::<usize>();

    let is_candidate = plain_text_len >= MIN_CONTENT_TEXT_CHARS;
    if !is_candidate {
        log_info!("SKIP (too few chars: {} < {}): {}", plain_text_len, MIN_CONTENT_TEXT_CHARS, file_name);
    }
    is_candidate
}

async fn translate_html_content(
    client: &Client,
    api_key: &str,
    model: Option<&str>,
    target_language: &str,
    html: &str,
    char_budget: Option<usize>,
) -> Result<(String, usize, usize), String> {
    let tokens = tokenize_html(html);
    let mut result = String::with_capacity(html.len());

    let mut attempted_characters = 0usize;
    let mut translated_characters = 0usize;
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

                let remaining = char_budget.map(|limit| limit.saturating_sub(attempted_characters));
                let (translated_text, attempted, translated, exhausted_now) =
                    translate_text_preserving_whitespace(
                        client,
                        api_key,
                        model,
                        target_language,
                        &token.value,
                        remaining,
                    )
                    .await?;

                attempted_characters += attempted;
                translated_characters += translated;
                if exhausted_now {
                    budget_exhausted = true;
                }

                result.push_str(&translated_text);
            }
        }
    }

    Ok((result, attempted_characters, translated_characters))
}

async fn translate_text_preserving_whitespace(
    client: &Client,
    api_key: &str,
    model: Option<&str>,
    target_language: &str,
    text: &str,
    remaining_chars: Option<usize>,
) -> Result<(String, usize, usize, bool), String> {
    if text.trim().is_empty() {
        return Ok((text.to_string(), 0, 0, false));
    }

    let trimmed = text.trim();
    let Some(start) = text.find(trimmed) else {
        return Ok((text.to_string(), 0, 0, false));
    };
    let end = start + trimmed.len();

    let leading = &text[..start];
    let trailing = &text[end..];

    let (input_to_translate, attempted, exhausted_now) = if let Some(remaining) = remaining_chars {
        if remaining == 0 {
            return Ok((text.to_string(), 0, 0, true));
        }

        let trimmed_chars = trimmed.chars().count();
        if trimmed_chars <= remaining {
            (trimmed.to_string(), trimmed_chars, false)
        } else {
            let (head, tail) = split_at_char_count(trimmed, remaining);
            if head.trim().is_empty() {
                return Ok((text.to_string(), 0, 0, true));
            }

            let head_chars = head.chars().count();

            let translated_head = ai::translate_text_with_retry(
                client,
                api_key,
                model,
                target_language,
                head,
                MAX_RETRIES,
            )
            .await;

            let translated_head = match translated_head {
                Ok(value) => value,
                Err(err) => {
                    log_info!(
                        "Chunk skipped (partial) and kept original due to translation error: {}",
                        err
                    );
                    return Ok((text.to_string(), head_chars, 0, true));
                }
            };

            if !is_meaningful_translation_change(head, &translated_head) {
                return Ok((text.to_string(), head_chars, 0, true));
            }

            return Ok((
                format!("{}{}{}{}", leading, translated_head, tail, trailing),
                head_chars,
                head_chars,
                true,
            ));
        }
    } else {
        (trimmed.to_string(), trimmed.chars().count(), false)
    };

    let translated = ai::translate_text_with_retry(
        client,
        api_key,
        model,
        target_language,
        &input_to_translate,
        MAX_RETRIES,
    )
    .await;

    let translated = match translated {
        Ok(value) => value,
        Err(err) => {
            log_info!("Chunk skipped and kept original due to translation error: {}", err);
            return Ok((text.to_string(), attempted, 0, exhausted_now));
        }
    };

    if !is_meaningful_translation_change(&input_to_translate, &translated) {
        return Ok((text.to_string(), attempted, 0, exhausted_now));
    }

    Ok((
        format!("{}{}{}", leading, translated, trailing),
        attempted,
        attempted,
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

fn is_meaningful_translation_change(source: &str, translated: &str) -> bool {
    let norm_source = normalize_for_change(source);
    let norm_translated = normalize_for_change(translated);

    if norm_source == norm_translated {
        log_info!("Translation IDENTICAL after normalization (rejected)");
        log_info!("  source:     '{}'", preview_chars(source, 120));
        log_info!("  translated: '{}'", preview_chars(translated, 120));
        return false;
    }

    log_info!("Translation ACCEPTED (normalized text differs)");
    true
}

fn normalize_for_change(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .collect()
}

fn preview_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect()
}

const CDATA_START: &str = "<![CDATA[";
const CDATA_END: &str = "]]>";

fn tokenize_html(html: &str) -> Vec<HtmlToken> {
    let mut tokens = Vec::new();
    let mut rest = html;

    while !rest.is_empty() {
        if let Some(pos) = rest.find(CDATA_START) {
            if pos > 0 {
                tokens.extend(tokenize_html_segment(&rest[..pos]));
            }
            let inner_start = pos + CDATA_START.len();
            if inner_start > rest.len() {
                tokens.extend(tokenize_html_segment(rest));
                break;
            }
            let after_inner = &rest[inner_start..];
            if let Some(end_rel) = after_inner.find(CDATA_END) {
                let inner = &after_inner[..end_rel];
                if !inner.is_empty() {
                    tokens.push(HtmlToken {
                        kind: HtmlTokenKind::Text,
                        value: inner.to_string(),
                    });
                }
                rest = &after_inner[end_rel + CDATA_END.len()..];
            } else {
                log_info!("Unclosed CDATA; parsing from marker as HTML segment");
                tokens.extend(tokenize_html_segment(&rest[pos..]));
                break;
            }
        } else {
            tokens.extend(tokenize_html_segment(rest));
            break;
        }
    }

    tokens
}

fn tokenize_html_segment(html: &str) -> Vec<HtmlToken> {
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
