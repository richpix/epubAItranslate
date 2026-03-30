use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{Read, Write};

use quick_xml::events::Event;
use quick_xml::reader::Reader;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
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

    let reading_order_paths = match get_epub_reading_order(&mut reader) {
        Ok(paths) => paths,
        Err(e) => {
            println!("Warning: Could not get reading order: {}", e);
            let total_files = reader.len();
            (0..total_files)
                .filter_map(|i| {
                    reader
                        .by_index(i)
                        .ok()
                        .and_then(|entry| is_html_path(entry.name()).then(|| entry.name().to_string()))
                })
                .collect::<Vec<_>>()
        }
    };

    let total_html_files = reading_order_paths.len();

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
    let mut processed_paths = HashSet::new();

    // Fase 1: Procesar en el orden de lectura
    for file_name in &reading_order_paths {
        let (options, bytes) = {
            let mut entry = match reader.by_name(file_name) {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            let mut options = zip::write::FileOptions::default().compression_method(entry.compression());
            if let Some(mode) = entry.unix_mode() {
                options = options.unix_permissions(mode);
            }

            let mut bytes = Vec::new();
            if let Err(e) = entry.read_to_end(&mut bytes) {
                println!("Error al leer entrada del epub {}: {}", file_name, e);
                continue;
            }
            (options, bytes)
        };

        if is_html_path(file_name) {
            let source_html = match String::from_utf8(bytes.clone()) {
                Ok(content) => content,
                Err(err) => {
                    writer
                        .start_file(file_name.as_str(), options)
                        .map_err(|e| format!("No se pudo crear entrada: {}", e))?;
                    writer
                        .write_all(&err.into_bytes())
                        .map_err(|e| format!("Error en ZIP: {}", e))?;
                    processed_paths.insert(file_name.clone());
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

            let percent = if total_html_files == 0 {
                100.0
            } else {
                (translated_html_files as f32 / total_html_files as f32) * 100.0
            };

            emit_progress(
                &app,
                TranslationProgressPayload {
                    status: "processing".to_string(),
                    message: format!("Traduciendo {}", file_name),
                    current_file: translated_html_files,
                    total_files: total_html_files,
                    percent,
                    translated_characters,
                },
            );
        } else {
            writer
                .start_file(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo crear entrada: {}", e))?;
            writer
                .write_all(&bytes)
                .map_err(|e| format!("Error escribiendo en ZIP: {}", e))?;
        }
        processed_paths.insert(file_name.clone());
    }

    // Fase 2: Copiar el resto de los archivos (imagenes, css, y archivos no listados en el spine)
    let total_files = reader.len();
    for index in 0..total_files {
        let (file_name, options, is_directory, bytes) = {
            let mut entry = match reader.by_index(index) {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            let name = entry.name().to_string();
            if processed_paths.contains(&name) {
                continue;
            }

            let mut options = zip::write::FileOptions::default().compression_method(entry.compression());
            if let Some(mode) = entry.unix_mode() {
                options = options.unix_permissions(mode);
            }

            if entry.is_dir() {
                (name, options, true, Vec::new())
            } else {
                let mut bytes = Vec::new();
                if let Err(_) = entry.read_to_end(&mut bytes) {
                    continue;
                }
                (name, options, false, bytes)
            }
        };

        if is_directory {
            let _ = writer.add_directory(file_name.as_str(), options);
        } else {
            if let Ok(()) = writer.start_file(file_name.as_str(), options) {
                let _ = writer.write_all(&bytes);
            }
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

fn get_epub_reading_order(archive: &mut ZipArchive<File>) -> Result<Vec<String>, String> {
    let mut file = archive
        .by_name("META-INF/container.xml")
        .map_err(|e| format!("No se pudo leer META-INF/container.xml: {}", e))?;
    let mut xml = String::new();
    file.read_to_string(&mut xml)
        .map_err(|e| format!("Error leyendo container.xml: {}", e))?;
    drop(file);

    let mut reader = Reader::from_str(&xml);
    let mut rootfile_path = None;
    let mut buf = Vec::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Empty(ref e)) | Ok(Event::Start(ref e)) => {
                if e.name().as_ref() == b"rootfile" {
                    for attr in e.attributes() {
                        if let Ok(a) = attr {
                            if a.key.as_ref() == b"full-path" {
                                rootfile_path = String::from_utf8(a.value.into_owned()).ok();
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => (),
        }
    }

    let rootfile_path = rootfile_path.ok_or_else(|| "No se encontro <rootfile>".to_string())?;

    let mut opf_file = archive
        .by_name(&rootfile_path)
        .map_err(|e| format!("No se encontro archivo OPF: {}", e))?;
    let mut opf_xml = String::new();
    opf_file
        .read_to_string(&mut opf_xml)
        .map_err(|e| format!("Error leyendo OPF: {}", e))?;
    drop(opf_file);

    let base_path = match rootfile_path.rfind('/') {
        Some(idx) => &rootfile_path[..idx + 1],
        None => "",
    };

    let mut parser = Reader::from_str(&opf_xml);
    let mut buf = Vec::new();

    let mut manifest = HashMap::new();
    let mut spine = Vec::new();
    let mut in_spine = false;

    loop {
        match parser.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) | Ok(Event::Empty(ref e)) => {
                // We must handle namespacing, e.g. `<opf:item ...>` or `<item ...>`
                let tag_name = e.name();
                let local_name = tag_name.into_inner();
                // strip namespaces correctly: usually `item` is `item` but just in case
                let is_tag = |name_bytes: &[u8], target: &[u8]| -> bool {
                    name_bytes == target || name_bytes.ends_with(&[b':'].iter().chain(target).copied().collect::<Vec<u8>>())
                };

                if is_tag(local_name, b"item") {
                    let mut id = None;
                    let mut href = None;
                    for attr in e.attributes() {
                        if let Ok(a) = attr {
                            if a.key.as_ref() == b"id" {
                                id = String::from_utf8(a.value.into_owned()).ok();
                            } else if a.key.as_ref() == b"href" {
                                href = String::from_utf8(a.value.into_owned()).ok();
                            }
                        }
                    }
                    if let (Some(id), Some(href)) = (id, href) {
                        manifest.insert(id, href);
                    }
                } else if is_tag(local_name, b"spine") {
                    in_spine = true;
                } else if is_tag(local_name, b"itemref") && in_spine {
                    for attr in e.attributes() {
                        if let Ok(a) = attr {
                            if a.key.as_ref() == b"idref" {
                                if let Ok(idref) = String::from_utf8(a.value.into_owned()) {
                                    spine.push(idref);
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::End(ref e)) => {
                let tag_name = e.name();
                let local_name = tag_name.into_inner();
                let is_tag = |name_bytes: &[u8], target: &[u8]| -> bool {
                    name_bytes == target || name_bytes.ends_with(&[b':'].iter().chain(target).copied().collect::<Vec<u8>>())
                };
                if is_tag(local_name, b"spine") {
                    in_spine = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => (),
        }
        buf.clear();
    }

    let mut reading_order = Vec::new();
    for idref in spine {
        if let Some(href) = manifest.get(&idref) {
            let full_path = format!("{}{}", base_path, href);
            reading_order.push(full_path);
        }
    }

    Ok(reading_order)
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
