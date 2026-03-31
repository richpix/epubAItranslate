use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::File;
use std::io::{Read, Write};

use futures_util::stream::{FuturesUnordered, StreamExt};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use zip::{ZipArchive, ZipWriter};

use crate::ai;

const APPROX_CHARS_PER_PAGE: usize = 1800;
const APPROX_CHARS_PER_TOKEN: usize = 4;
const DEFAULT_PREVIEW_PAGES: usize = 5;
const CHAPTER_TOKEN_THRESHOLD: usize = 10_000;
const CHUNK_MIN_CHARS: usize = 2_000;
const CHUNK_TARGET_CHARS: usize = 3_000;
const CHUNK_MAX_CHARS: usize = 4_000;
const FULL_HTML_BLOCK_MIN_CHARS: usize = 8_000;
const FULL_HTML_BLOCK_TARGET_CHARS: usize = 12_000;
const FULL_HTML_BLOCK_MAX_CHARS: usize = 18_000;
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 3;
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

#[derive(Clone, Copy)]
struct TranslationChunkOptions {
    enable_chunking: bool,
    chunk_threshold_chars: usize,
    chunk_min_chars: usize,
    chunk_target_chars: usize,
    max_chunk_chars: usize,
    enable_streaming: bool,
    max_concurrent_requests: usize,
    dynamic_rate_limit: bool,
}

#[derive(Clone, Copy)]
struct EntryMeta {
    compression: zip::CompressionMethod,
    unix_mode: Option<u32>,
}

enum SpineEntryContent {
    Html(String),
    Raw(Vec<u8>),
}

struct SpineEntry {
    file_name: String,
    meta: EntryMeta,
    content: SpineEntryContent,
}

struct ProgressReporter {
    app: tauri::AppHandle,
    file_name: String,
    current_file: usize,
    total_files: usize,
    completed_files: usize,
    base_translated_chars: usize,
}

impl ProgressReporter {
    fn emit(&self, consumed_chars: usize, file_fraction: f32, message: &str) {
        let percent = if self.total_files == 0 {
            100.0
        } else {
            let fraction = file_fraction.clamp(0.0, 1.0);
            ((self.completed_files as f32 + fraction) / self.total_files as f32) * 100.0
        };

        emit_progress(
            &self.app,
            TranslationProgressPayload {
                status: "processing".to_string(),
                message: message.to_string(),
                current_file: self.current_file,
                total_files: self.total_files,
                percent,
                translated_characters: self.base_translated_chars + consumed_chars,
            },
        );
    }
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

    let output_file = File::create(output_path)
        .map_err(|e| format!("No se pudo crear el EPUB de salida: {}", e))?;
    let mut writer = ZipWriter::new(output_file);
    let client = Client::builder()
        .pool_max_idle_per_host(64)
        .tcp_nodelay(true)
        .build()
        .map_err(|e| format!("No se pudo inicializar cliente HTTP: {}", e))?;

    let preview_pages = request
        .preview_pages
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_PREVIEW_PAGES)
        .max(1);
    let preview_limit = preview_pages * APPROX_CHARS_PER_PAGE;

    let chunk_options = TranslationChunkOptions {
        enable_chunking: !request.preview_only,
        chunk_threshold_chars: CHAPTER_TOKEN_THRESHOLD * APPROX_CHARS_PER_TOKEN,
        chunk_min_chars: CHUNK_MIN_CHARS,
        chunk_target_chars: CHUNK_TARGET_CHARS,
        max_chunk_chars: CHUNK_MAX_CHARS,
        enable_streaming: request.preview_only,
        max_concurrent_requests: if request.preview_only {
            1
        } else {
            DEFAULT_MAX_CONCURRENT_REQUESTS
        },
        dynamic_rate_limit: !request.preview_only,
    };

    let mut translated_html_files = 0usize;
    let mut translated_characters = 0usize;
    let mut processed_paths = HashSet::new();
    let mut spine_entries = Vec::new();

    for file_name in &reading_order_paths {
        let (meta, bytes) = {
            let mut entry = match reader.by_name(file_name) {
                Ok(entry) => entry,
                Err(_) => continue,
            };

            let meta = EntryMeta {
                compression: entry.compression(),
                unix_mode: entry.unix_mode(),
            };

            let mut bytes = Vec::new();
            if let Err(e) = entry.read_to_end(&mut bytes) {
                println!("Error al leer entrada del epub {}: {}", file_name, e);
                continue;
            }
            (meta, bytes)
        };

        let content = if is_html_path(file_name) {
            match String::from_utf8(bytes) {
                Ok(source_html) => SpineEntryContent::Html(source_html),
                Err(err) => SpineEntryContent::Raw(err.into_bytes()),
            }
        } else {
            SpineEntryContent::Raw(bytes)
        };

        spine_entries.push(SpineEntry {
            file_name: file_name.clone(),
            meta,
            content,
        });
        processed_paths.insert(file_name.clone());
    }

    let total_html_files = spine_entries
        .iter()
        .filter(|entry| matches!(entry.content, SpineEntryContent::Html(_)))
        .count();

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

    if request.preview_only {
        for entry in &spine_entries {
            let file_options = build_file_options(entry.meta);
            match &entry.content {
                SpineEntryContent::Html(source_html) => {
                    let should_translate = translated_characters < preview_limit;
                    let (translated_html, consumed_chars) = if should_translate {
                        let reporter = ProgressReporter {
                            app: app.clone(),
                            file_name: entry.file_name.clone(),
                            current_file: translated_html_files + 1,
                            total_files: total_html_files,
                            completed_files: translated_html_files,
                            base_translated_chars: translated_characters,
                        };

                        translate_html_content(
                            &client,
                            request.api_key.trim(),
                            language_label(&request.target_language),
                            source_html,
                            Some(preview_limit.saturating_sub(translated_characters)),
                            &chunk_options,
                            Some(&reporter),
                        )
                        .await?
                    } else {
                        (source_html.clone(), 0)
                    };

                    translated_characters += consumed_chars;
                    translated_html_files += 1;

                    writer
                        .start_file(entry.file_name.as_str(), file_options)
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
                            message: format!("Traduciendo {}", entry.file_name),
                            current_file: translated_html_files,
                            total_files: total_html_files,
                            percent,
                            translated_characters,
                        },
                    );
                }
                SpineEntryContent::Raw(bytes) => {
                    writer
                        .start_file(entry.file_name.as_str(), file_options)
                        .map_err(|e| format!("No se pudo crear entrada: {}", e))?;
                    writer
                        .write_all(bytes)
                        .map_err(|e| format!("Error escribiendo en ZIP: {}", e))?;
                }
            }
        }
    } else {
        let mut html_indices = Vec::new();
        for (idx, entry) in spine_entries.iter().enumerate() {
            if let SpineEntryContent::Html(_) = entry.content {
                html_indices.push(idx);
            }
        }
        let html_total = html_indices.len();

        let (tx, rx) = std::sync::mpsc::channel::<(usize, String)>();
        
        let mut writer_local = writer;
        let mut expected_index = 0usize;
        let mut buffered_html: HashMap<usize, String> = HashMap::new();
        
        let mut original_html_contents = HashMap::new();
        let mut original_file_names = HashMap::new();
        for (idx, entry) in spine_entries.iter().enumerate() {
            if let SpineEntryContent::Html(ref content) = entry.content {
                original_html_contents.insert(idx, content.clone());
            }
            original_file_names.insert(idx, entry.file_name.clone());
        }
        
        // Destruct the vector out of logic loop
        let spine_entries_for_writer = std::mem::replace(&mut spine_entries, Vec::new());
        let writer_thread = std::thread::spawn(move || -> Result<ZipWriter<File>, String> {
            while expected_index < spine_entries_for_writer.len() {
                let entry = &spine_entries_for_writer[expected_index];
                let file_options = build_file_options(entry.meta);
                
                match &entry.content {
                    SpineEntryContent::Raw(bytes) => {
                        writer_local.start_file(entry.file_name.as_str(), file_options).map_err(|e| e.to_string())?;
                        writer_local.write_all(bytes).map_err(|e| e.to_string())?;
                        expected_index += 1;
                    }
                    SpineEntryContent::Html(_) => {
                        if let Some(html) = buffered_html.remove(&expected_index) {
                            writer_local.start_file(entry.file_name.as_str(), file_options).map_err(|e| e.to_string())?;
                            writer_local.write_all(html.as_bytes()).map_err(|e| e.to_string())?;
                            expected_index += 1;
                        } else {
                            if let Ok((idx, html)) = rx.recv() {
                                if idx == expected_index {
                                    writer_local.start_file(entry.file_name.as_str(), file_options).map_err(|e| e.to_string())?;
                                    writer_local.write_all(html.as_bytes()).map_err(|e| e.to_string())?;
                                    expected_index += 1;
                                } else {
                                    buffered_html.insert(idx, html);
                                }
                            } else {
                                break;
                            }
                        }
                    }
                }
            }
            Ok(writer_local)
        });

        let mut pending = VecDeque::from(html_indices);
        let mut in_flight = FuturesUnordered::new();
        let mut retries: HashMap<usize, u32> = HashMap::new();
        let max_concurrency = chunk_options.max_concurrent_requests.max(1);
        let mut active_concurrency = max_concurrency;
        let mut success_streak = 0usize;

        while !pending.is_empty() || !in_flight.is_empty() {
            while in_flight.len() < active_concurrency && !pending.is_empty() {
                let Some(chapter_index) = pending.pop_front() else {
                    break;
                };

                let source_html = original_html_contents.get(&chapter_index).unwrap().clone();
                let file_name = original_file_names.get(&chapter_index).unwrap().clone();
                let client = client.clone();
                let api_key = request.api_key.trim().to_string();
                let target_language = language_label(&request.target_language).to_string();
                let options = chunk_options;

                in_flight.push(async move {
                    let translated = translate_html_content(
                        &client,
                        &api_key,
                        &target_language,
                        &source_html,
                        None,
                        &options,
                        None,
                    )
                    .await;
                    (chapter_index, file_name, translated)
                });
            }

            let Some((chapter_index, file_name, result)) = in_flight.next().await else {
                break;
            };

            match result {
                Ok((translated_html, consumed_chars)) => {
                    let _ = tx.send((chapter_index, translated_html));
                    translated_html_files += 1;
                    translated_characters += consumed_chars;
                    success_streak += 1;

                    if chunk_options.dynamic_rate_limit
                        && active_concurrency < max_concurrency
                        && success_streak >= 3
                    {
                        active_concurrency += 1;
                        success_streak = 0;
                    }

                    let percent = if html_total == 0 {
                        100.0
                    } else {
                        (translated_html_files as f32 / html_total as f32) * 100.0
                    };

                    emit_progress(
                        &app,
                        TranslationProgressPayload {
                            status: "processing".to_string(),
                            message: format!(
                                "Traduciendo a disco {} ({} hilos)",
                                file_name, active_concurrency
                            ),
                            current_file: translated_html_files,
                            total_files: html_total,
                            percent,
                            translated_characters,
                        },
                    );
                }
                Err(err) => {
                    let retry_count = retries.get(&chapter_index).copied().unwrap_or(0);
                    if chunk_options.dynamic_rate_limit
                        && is_rate_limit_error(&err)
                        && retry_count < MAX_RETRIES
                    {
                        retries.insert(chapter_index, retry_count + 1);
                        pending.push_back(chapter_index);
                        success_streak = 0;
                        if active_concurrency > 1 {
                            active_concurrency -= 1;
                        }

                        let backoff_secs = 2u64.pow(retry_count as u32);
                        tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;

                        emit_progress(
                            &app,
                            TranslationProgressPayload {
                                status: "processing".to_string(),
                                message: format!(
                                    "Rate limit, esperando {}s",
                                    backoff_secs
                                ),
                                current_file: translated_html_files,
                                total_files: html_total,
                                percent: if html_total == 0 {
                                    100.0
                                } else {
                                    (translated_html_files as f32 / html_total as f32) * 100.0
                                },
                                translated_characters,
                            },
                        );
                        continue;
                    }

                    return Err(format!("Error traduciendo {}: {}", file_name, err));
                }
            }
        }

        drop(tx);
        writer = writer_thread.join().map_err(|_| "Error en el worker the disco".to_string())??;
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

fn build_file_options(meta: EntryMeta) -> zip::write::FileOptions {
    let mut options = zip::write::FileOptions::default().compression_method(meta.compression);
    if let Some(mode) = meta.unix_mode {
        options = options.unix_permissions(mode);
    }
    options
}

fn is_rate_limit_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("429") || lower.contains("rate limit") || lower.contains("too many requests")
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
                let tag_name = e.name();
                let local_name = tag_name.into_inner();
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

fn emit_progress_for_reporter(
    reporter: Option<&ProgressReporter>,
    total_text_chars: usize,
    consumed_chars: usize,
    message: &str,
) {
    if let Some(active_reporter) = reporter {
        let file_fraction = if total_text_chars == 0 {
            0.0
        } else {
            (consumed_chars as f32 / total_text_chars as f32).min(1.0)
        };
        active_reporter.emit(consumed_chars, file_fraction, message);
    }
}

async fn translate_html_content(
    client: &Client,
    api_key: &str,
    target_language: &str,
    html: &str,
    char_budget: Option<usize>,
    options: &TranslationChunkOptions,
    reporter: Option<&ProgressReporter>,
) -> Result<(String, usize), String> {
    // Fast path for full-book mode: translate larger HTML blocks to reduce
    // drastically the number of API calls.
    if !options.enable_streaming && char_budget.is_none() {
        return translate_html_in_blocks(client, api_key, target_language, html).await;
    }

    let tokens = tokenize_html(html);
    let mut result = String::with_capacity(html.len());

    let mut consumed_characters = 0usize;
    let mut skip_tag_depth = 0usize;
    let mut budget_exhausted = false;
    let total_text_chars = count_translatable_text_chars(&tokens);
    let enable_chunking = options.enable_chunking && total_text_chars > options.chunk_threshold_chars;
    let progress_label = if let Some(active_reporter) = reporter {
        format!("Traduciendo {}", active_reporter.file_name)
    } else {
        "Traduciendo".to_string()
    };

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
                        options,
                        enable_chunking,
                        consumed_characters,
                        reporter,
                        total_text_chars,
                        progress_label.as_str(),
                    )
                    .await?;

                consumed_characters += consumed;
                if exhausted_now {
                    budget_exhausted = true;
                }

                result.push_str(&translated_text);
                emit_progress_for_reporter(
                    reporter,
                    total_text_chars,
                    consumed_characters,
                    progress_label.as_str(),
                );
            }
        }
    }

    Ok((result, consumed_characters))
}

async fn translate_html_in_blocks(
    client: &Client,
    api_key: &str,
    target_language: &str,
    html: &str,
) -> Result<(String, usize), String> {
    let tokens = tokenize_html(html);
    let consumed = count_translatable_text_chars(&tokens);
    if consumed == 0 {
        return Ok((html.to_string(), 0));
    }

    let blocks = split_html_into_blocks(
        &tokens,
        FULL_HTML_BLOCK_MIN_CHARS,
        FULL_HTML_BLOCK_TARGET_CHARS,
        FULL_HTML_BLOCK_MAX_CHARS,
    );
    if blocks.is_empty() {
        return Ok((html.to_string(), consumed));
    }

    let mut translated = String::with_capacity(html.len());
    for block in blocks {
        let chunk = ai::translate_text_with_retry(
            client,
            api_key,
            target_language,
            block.as_str(),
            MAX_RETRIES,
        )
        .await?;
        translated.push_str(&chunk);
    }

    Ok((translated, consumed))
}

async fn translate_text_preserving_whitespace(
    client: &Client,
    api_key: &str,
    target_language: &str,
    text: &str,
    remaining_chars: Option<usize>,
    options: &TranslationChunkOptions,
    use_chunking: bool,
    consumed_before: usize,
    reporter: Option<&ProgressReporter>,
    total_text_chars: usize,
    progress_label: &str,
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

            let translated_head = {
                if options.enable_streaming {
                    let mut on_delta = |_: &str| {
                        emit_progress_for_reporter(
                            reporter,
                            total_text_chars,
                            consumed_before,
                            progress_label,
                        );
                    };
                    translate_text(
                        client,
                        api_key,
                        target_language,
                        head,
                        options,
                        Some(&mut on_delta),
                    )
                    .await?
                } else {
                    translate_text(client, api_key, target_language, head, options, None).await?
                }
            };

            return Ok((
                format!("{}{}{}{}", leading, translated_head, tail, trailing),
                remaining,
                true,
            ));
        }
    } else {
        (trimmed.to_string(), trimmed.chars().count(), false)
    };

    let translated = if use_chunking {
        let chunks = split_text_by_sentence(
            &input_to_translate,
            options.chunk_min_chars,
            options.chunk_target_chars,
            options.max_chunk_chars,
        );
        let ranges = if chunks.is_empty() {
            vec![(0, input_to_translate.len())]
        } else {
            chunks
        };

        let total_chunks = ranges.len();
        let mut translated_chunks = String::new();
        let mut consumed_local = 0usize;

        for (index, (start, end)) in ranges.into_iter().enumerate() {
            let chunk = &input_to_translate[start..end];
            let chunk_message = if total_chunks > 1 {
                format!("{} (fragmento {}/{})", progress_label, index + 1, total_chunks)
            } else {
                progress_label.to_string()
            };
            let chunk_message_ref = chunk_message.as_str();

            let translated_chunk = {
                if options.enable_streaming {
                    let mut on_delta = |_: &str| {
                        emit_progress_for_reporter(
                            reporter,
                            total_text_chars,
                            consumed_before + consumed_local,
                            chunk_message_ref,
                        );
                    };
                    translate_text(
                        client,
                        api_key,
                        target_language,
                        chunk,
                        options,
                        Some(&mut on_delta),
                    )
                    .await?
                } else {
                    translate_text(client, api_key, target_language, chunk, options, None).await?
                }
            };

            translated_chunks.push_str(&translated_chunk);
            consumed_local += chunk.chars().count();
            emit_progress_for_reporter(
                reporter,
                total_text_chars,
                consumed_before + consumed_local,
                chunk_message_ref,
            );
        }

        translated_chunks
    } else {
        if options.enable_streaming {
            let mut on_delta = |_: &str| {
                emit_progress_for_reporter(
                    reporter,
                    total_text_chars,
                    consumed_before,
                    progress_label,
                );
            };
            translate_text(
                client,
                api_key,
                target_language,
                &input_to_translate,
                options,
                Some(&mut on_delta),
            )
            .await?
        } else {
            translate_text(
                client,
                api_key,
                target_language,
                &input_to_translate,
                options,
                None,
            )
            .await?
        }
    };

    Ok((
        format!("{}{}{}", leading, translated, trailing),
        consumed,
        exhausted_now,
    ))
}

async fn translate_text(
    client: &Client,
    api_key: &str,
    target_language: &str,
    text: &str,
    options: &TranslationChunkOptions,
    on_delta: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<String, String> {
    if options.enable_streaming {
        ai::translate_text_with_retry_streaming(
            client,
            api_key,
            target_language,
            text,
            MAX_RETRIES,
            on_delta,
        )
        .await
    } else {
        ai::translate_text_with_retry(client, api_key, target_language, text, MAX_RETRIES).await
    }
}

fn count_translatable_text_chars(tokens: &[HtmlToken]) -> usize {
    let mut total = 0usize;
    let mut skip_tag_depth = 0usize;

    for token in tokens {
        match token.kind {
            HtmlTokenKind::Tag => update_skip_depth(&token.value, &mut skip_tag_depth),
            HtmlTokenKind::Text => {
                if skip_tag_depth == 0 {
                    total += token.value.chars().count();
                }
            }
        }
    }

    total
}

fn split_text_by_sentence(
    text: &str,
    min_chars: usize,
    target_chars: usize,
    max_chars: usize,
) -> Vec<(usize, usize)> {
    let mut segments = Vec::new();
    if max_chars == 0 || text.is_empty() {
        return segments;
    }

    let mut start = 0usize;
    while start < text.len() {
        let mut char_count = 0usize;
        let mut last_sentence_end: Option<usize> = None;
        let mut end = text.len();

        for (offset, ch) in text[start..].char_indices() {
            char_count += 1;
            let idx = start + offset;

            if matches!(ch, '.' | '!' | '?' | '\n') {
                last_sentence_end = Some(idx + ch.len_utf8());
            }

            if char_count >= target_chars && char_count >= min_chars {
                if let Some(sentence_end) = last_sentence_end {
                    end = sentence_end;
                    break;
                }
            }

            if char_count >= max_chars {
                end = last_sentence_end.unwrap_or_else(|| idx + ch.len_utf8());
                break;
            }
        }

        if end <= start {
            break;
        }

        segments.push((start, end));
        start = end;
    }

    segments
}

fn split_html_into_blocks(
    tokens: &[HtmlToken],
    min_chars: usize,
    target_chars: usize,
    max_chars: usize,
) -> Vec<String> {
    if tokens.is_empty() {
        return Vec::new();
    }

    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut current_chars = 0usize;
    let mut skip_tag_depth = 0usize;

    for token in tokens {
        match token.kind {
            HtmlTokenKind::Tag => {
                update_skip_depth(&token.value, &mut skip_tag_depth);
                current.push_str(&token.value);

                let should_split_at_boundary = current_chars >= target_chars
                    && current_chars >= min_chars
                    && is_html_boundary_tag(&token.value);
                let should_force_split = current_chars >= max_chars;

                if should_split_at_boundary || should_force_split {
                    blocks.push(current);
                    current = String::new();
                    current_chars = 0;
                }
            }
            HtmlTokenKind::Text => {
                current.push_str(&token.value);
                if skip_tag_depth == 0 && !token.value.trim().is_empty() {
                    current_chars += token.value.chars().count();
                }
            }
        }
    }

    if !current.is_empty() {
        blocks.push(current);
    }

    blocks
}

fn is_html_boundary_tag(tag: &str) -> bool {
    let lower = tag.to_ascii_lowercase();
    lower.starts_with("</p")
        || lower.starts_with("</div")
        || lower.starts_with("</section")
        || lower.starts_with("</article")
        || lower.starts_with("</li")
        || lower.starts_with("</h1")
        || lower.starts_with("</h2")
        || lower.starts_with("</h3")
        || lower.starts_with("</h4")
        || lower.starts_with("</blockquote")
        || lower.starts_with("</br")
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
