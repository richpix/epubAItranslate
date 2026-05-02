use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use zip::{ZipArchive, ZipWriter};

use crate::ai;

const APPROX_CHARS_PER_PAGE: usize = 1800;
const APPROX_CHARS_PER_TOKEN: usize = 4;
const DEFAULT_PREVIEW_PAGES: usize = 5;
const CHAPTER_TOKEN_THRESHOLD: usize = 10_000;
const CHUNK_MIN_CHARS: usize = 2_000;
const CHUNK_TARGET_CHARS: usize = 3_000;
const CHUNK_MAX_CHARS: usize = 4_000;
const FULL_HTML_BLOCK_MIN_CHARS: usize = 12_000;
const FULL_HTML_BLOCK_TARGET_CHARS: usize = 16_000;
const FULL_HTML_BLOCK_MAX_CHARS: usize = 20_000;
const RECOVERY_BLOCK_MIN_CHARS: usize = 6_000;
const RECOVERY_BLOCK_TARGET_CHARS: usize = 10_000;
const RECOVERY_BLOCK_MAX_CHARS: usize = 14_000;
const ADAPTIVE_TRUNCATION_MAX_DEPTH: u8 = 3;
const ADAPTIVE_TRUNCATION_MIN_CHARS: usize = 1_000;
const TRUNCATION_RECOVERY_CHUNK_MIN_CHARS: usize = 800;
const TRUNCATION_RECOVERY_CHUNK_TARGET_CHARS: usize = 1_200;
const TRUNCATION_RECOVERY_CHUNK_MAX_CHARS: usize = 1_800;
const DEFAULT_MAX_CONCURRENT_REQUESTS: usize = 10;
const DEFAULT_CONCURRENCY_WARMUP: usize = 6;
const SUCCESS_STREAK_FOR_SCALE_UP: usize = 1;
const MAX_RETRIES: u32 = 4;
const MAX_RETRIES_FOR_DECODE_ERROR: u32 = 1;
const MAX_DECODE_ERROR_FALLBACK_CHAPTERS: usize = 8;
const SAFE_TAIL_HTML_FILES: usize = 0;
const MAX_CONCURRENCY_ENV: &str = "EPUBTR_MAX_CONCURRENCY";
const FULL_BLOCK_MIN_ENV: &str = "EPUBTR_FULL_BLOCK_MIN_CHARS";
const FULL_BLOCK_TARGET_ENV: &str = "EPUBTR_FULL_BLOCK_TARGET_CHARS";
const FULL_BLOCK_MAX_ENV: &str = "EPUBTR_FULL_BLOCK_MAX_CHARS";
static TRANSLATION_RUN_ACTIVE: AtomicBool = AtomicBool::new(false);
static BLOCK_TRANSLATION_CACHE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<u64, String>>> = std::sync::OnceLock::new();

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslateEpubRequest {
    pub input_path: String,
    pub output_path: String,
    pub target_language: String,
    pub api_key: String,
    pub provider: String,
    pub model: String,
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

#[derive(Clone)]
struct TranslationChunkOptions {
    provider: String,
    model: String,
    enable_chunking: bool,
    chunk_threshold_chars: usize,
    chunk_min_chars: usize,
    chunk_target_chars: usize,
    max_chunk_chars: usize,
    enable_streaming: bool,
    max_concurrent_requests: usize,
    dynamic_rate_limit: bool,
    full_block_min_chars: usize,
    full_block_target_chars: usize,
    full_block_max_chars: usize,
    force_text_node_mode: bool,
}

#[derive(Clone, Copy)]
struct EntryMeta {
    compression: zip::CompressionMethod,
    #[cfg(unix)]
    unix_mode: Option<u32>,
}

enum SpineEntryContent {
    Html(Arc<str>),
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

struct TranslationRunGuard;

impl Drop for TranslationRunGuard {
    fn drop(&mut self) {
        TRANSLATION_RUN_ACTIVE.store(false, Ordering::Release);
    }
}

fn acquire_translation_run_guard() -> Result<TranslationRunGuard, String> {
    TRANSLATION_RUN_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            "Ya hay una traduccion en curso. Espera a que termine antes de iniciar otra."
                .to_string()
        })?;

    Ok(TranslationRunGuard)
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

// Punto de entrada principal (Comando Tauri) para iniciar el pipeline de traducción.
// 1. Descomprime un archivo epub de origen.
// 2. Extrae el orden de lectura recorriendo en XML 'container.xml' y el 'content.opf' para descubrir los ficheros html correctos del 'spine'.
// 3. Levanta la concurrencia definida para enviar las partes (chunks/text/blocks) a DeepSeek.
// 4. Comprime un nuevo .epub empaquetando cada fichero manteniendo la estructura original e insertando el HTML modificado.
#[tauri::command]
pub async fn translate_epub(
    app: tauri::AppHandle,
    request: TranslateEpubRequest,
) -> Result<TranslateEpubResult, String> {
    let overall_start = Instant::now();
    let _translation_run_guard = acquire_translation_run_guard()?;
    let input_path_raw = request.input_path.trim();
    let output_path_raw = request.output_path.trim();

    if input_path_raw.is_empty() || output_path_raw.is_empty() {
        return Err("Las rutas de entrada y salida son obligatorias".to_string());
    }

    if request.api_key.trim().is_empty() {
        return Err("La API key de DeepSeek es obligatoria".to_string());
    }

    if !input_path_raw.to_lowercase().ends_with(".epub") {
        return Err("Solo se admiten archivos .epub".to_string());
    }

    let input_path = Path::new(input_path_raw);
    let output_path = Path::new(output_path_raw);

    let canonical_input = input_path
        .canonicalize()
        .map_err(|e| format!("No se pudo resolver la ruta de entrada: {}", e))?;
    let absolute_output = make_absolute_path(output_path)
        .map_err(|e| format!("No se pudo resolver la ruta de salida: {}", e))?;

    if canonical_input == absolute_output {
        return Err("La ruta de salida no puede ser igual a la de entrada".to_string());
    }

    if let Some(parent) = absolute_output.parent() {
        if !parent.exists() {
            return Err(format!(
                "La carpeta de salida no existe: {}",
                parent.display()
            ));
        }
    }

    let mut reader = ZipArchive::new(
        File::open(&canonical_input).map_err(|e| format!("No se pudo abrir el EPUB: {}", e))?,
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

    let output_file = File::create(&absolute_output)
        .map_err(|e| format!("No se pudo crear el EPUB de salida: {}", e))?;
    let mut writer = ZipWriter::new(output_file);
    let client = Client::builder()
        .pool_max_idle_per_host(64)
        .tcp_nodelay(true)
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()
        .map_err(|e| format!("No se pudo inicializar cliente HTTP: {}", e))?;

    let preview_pages = request
        .preview_pages
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_PREVIEW_PAGES)
        .max(1);
    let preview_limit = preview_pages * APPROX_CHARS_PER_PAGE;

    let configured_max_concurrency = if request.preview_only {
        1
    } else {
        read_env_usize(MAX_CONCURRENCY_ENV, DEFAULT_MAX_CONCURRENT_REQUESTS, 1, 16)
    };
    let mut full_block_min_chars =
        read_env_usize(FULL_BLOCK_MIN_ENV, FULL_HTML_BLOCK_MIN_CHARS, 8_000, 120_000);
    let mut full_block_target_chars = read_env_usize(
        FULL_BLOCK_TARGET_ENV,
        FULL_HTML_BLOCK_TARGET_CHARS,
        8_000,
        120_000,
    );
    let mut full_block_max_chars =
        read_env_usize(FULL_BLOCK_MAX_ENV, FULL_HTML_BLOCK_MAX_CHARS, 8_000, 120_000);

    if full_block_min_chars > full_block_max_chars {
        std::mem::swap(&mut full_block_min_chars, &mut full_block_max_chars);
    }
    full_block_target_chars =
        full_block_target_chars.clamp(full_block_min_chars, full_block_max_chars);

    let chunk_options = TranslationChunkOptions {
        provider: request.provider.clone(),
        model: request.model.clone(),
        enable_chunking: !request.preview_only,
        chunk_threshold_chars: CHAPTER_TOKEN_THRESHOLD * APPROX_CHARS_PER_TOKEN,
        chunk_min_chars: CHUNK_MIN_CHARS,
        chunk_target_chars: CHUNK_TARGET_CHARS,
        max_chunk_chars: CHUNK_MAX_CHARS,
        enable_streaming: request.preview_only,
        max_concurrent_requests: configured_max_concurrency,
        dynamic_rate_limit: !request.preview_only,
        full_block_min_chars,
        full_block_target_chars,
        full_block_max_chars,
        force_text_node_mode: false,
    };

    let mut translated_html_files = 0usize;
    let mut translated_characters = 0usize;
    let mut fallback_html_files = 0usize;
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
                #[cfg(unix)]
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
                Ok(source_html) => SpineEntryContent::Html(Arc::<str>::from(source_html)),
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

    println!(
        "EPUBTR PERF: preparacion {:.2}s (html_files={}, concurrency={}, full_blocks={}..{}..{})",
        overall_start.elapsed().as_secs_f32(),
        total_html_files,
        chunk_options.max_concurrent_requests,
        chunk_options.full_block_min_chars,
        chunk_options.full_block_target_chars,
        chunk_options.full_block_max_chars
    );

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
                            source_html.as_ref(),
                            Some(preview_limit.saturating_sub(translated_characters)),
                            &chunk_options,
                            Some(&reporter),
                        )
                        .await?
                    } else {
                        (source_html.to_string(), 0)
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

        let tail_safe_html_indices: HashSet<usize> = if html_total > 0 && SAFE_TAIL_HTML_FILES > 0 {
            let tail_start = html_total.saturating_sub(SAFE_TAIL_HTML_FILES);
            html_indices.iter().skip(tail_start).copied().collect()
        } else {
            HashSet::new()
        };

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
        let writer_thread = std::thread::spawn(move || -> Result<(ZipWriter<File>, usize), String> {
            let mut writer_fallbacks = 0usize;
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
                            match rx.recv() {
                                Ok((idx, html)) => {
                                    if idx == expected_index {
                                        writer_local.start_file(entry.file_name.as_str(), file_options).map_err(|e| e.to_string())?;
                                        writer_local.write_all(html.as_bytes()).map_err(|e| e.to_string())?;
                                        expected_index += 1;
                                    } else {
                                        buffered_html.insert(idx, html);
                                    }
                                }
                                Err(_) => {
                                    // The producer stopped unexpectedly; write original HTML
                                    // so the output EPUB remains complete instead of truncated.
                                    if let SpineEntryContent::Html(original_html) = &entry.content {
                                        writer_local.start_file(entry.file_name.as_str(), file_options).map_err(|e| e.to_string())?;
                                        writer_local.write_all(original_html.as_bytes()).map_err(|e| e.to_string())?;
                                        writer_fallbacks += 1;
                                        expected_index += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok((writer_local, writer_fallbacks))
        });

        let mut pending = VecDeque::from(html_indices);
        let mut in_flight = FuturesUnordered::new();
        let mut retries: HashMap<usize, u32> = HashMap::new();
        let mut decode_recovery_chapters: HashSet<usize> = HashSet::new();
        let mut decode_fallback_chapters = 0usize;
        let max_concurrency = chunk_options.max_concurrent_requests.max(1);
        let mut active_concurrency = max_concurrency.min(DEFAULT_CONCURRENCY_WARMUP).max(1);
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
                let mut options = chunk_options.clone();

                if decode_recovery_chapters.contains(&chapter_index) {
                    options.full_block_min_chars = TRUNCATION_RECOVERY_CHUNK_MIN_CHARS;
                    options.full_block_target_chars = TRUNCATION_RECOVERY_CHUNK_TARGET_CHARS;
                    options.full_block_max_chars = TRUNCATION_RECOVERY_CHUNK_MAX_CHARS;
                    options.chunk_threshold_chars = 1;
                    options.chunk_min_chars = TRUNCATION_RECOVERY_CHUNK_MIN_CHARS;
                    options.chunk_target_chars = TRUNCATION_RECOVERY_CHUNK_TARGET_CHARS;
                    options.max_chunk_chars = TRUNCATION_RECOVERY_CHUNK_MAX_CHARS;
                }

                if tail_safe_html_indices.contains(&chapter_index) {
                    options.full_block_min_chars = TRUNCATION_RECOVERY_CHUNK_MIN_CHARS;
                    options.full_block_target_chars = TRUNCATION_RECOVERY_CHUNK_TARGET_CHARS;
                    options.full_block_max_chars = TRUNCATION_RECOVERY_CHUNK_MAX_CHARS;
                }

                in_flight.push(async move {
                    let translated = translate_html_content(
                        &client,
                        &api_key,
                        &target_language,
                        source_html.as_ref(),
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
                    retries.remove(&chapter_index);
                    decode_recovery_chapters.remove(&chapter_index);

                    if chunk_options.dynamic_rate_limit
                        && active_concurrency < max_concurrency
                        && success_streak >= SUCCESS_STREAK_FOR_SCALE_UP
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
                    let is_decode_error = is_response_decode_error(&err);
                    let retry_budget = if is_decode_error {
                        MAX_RETRIES_FOR_DECODE_ERROR
                    } else {
                        MAX_RETRIES
                    };
                    let is_retryable =
                        is_decode_error || is_rate_limit_error(&err) || is_transient_transport_error(&err);

                    if chunk_options.dynamic_rate_limit
                        && is_retryable
                        && retry_count < retry_budget
                    {
                        if is_decode_error {
                            decode_recovery_chapters.insert(chapter_index);
                        }

                        retries.insert(chapter_index, retry_count + 1);
                        pending.push_back(chapter_index);
                        success_streak = 0;
                        if active_concurrency > 1 {
                            active_concurrency = active_concurrency.saturating_sub(1).max(1);
                        }

                        let backoff_secs = if is_decode_error {
                            1
                        } else {
                            let exponential = 2u64.pow(retry_count.min(4));
                            let jitter_seed = (chapter_index as u64)
                                ^ ((retry_count as u64 + 1) * 0x9E37_79B9_7F4A_7C15);
                            let jitter = jitter_seed.rotate_left(17) % 5;
                            (exponential + jitter).clamp(1, 30)
                        };
                        tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;

                        emit_progress(
                            &app,
                            TranslationProgressPayload {
                                status: "processing".to_string(),
                                message: if is_decode_error {
                                    format!(
                                        "Respuesta invalida de DeepSeek, activando recovery por fragmentos y reintentando en {}s (concurrencia {})",
                                        backoff_secs,
                                        active_concurrency
                                    )
                                } else if is_rate_limit_error(&err) {
                                    format!(
                                        "Rate limit, esperando {}s (concurrencia {})",
                                        backoff_secs,
                                        active_concurrency
                                    )
                                } else {
                                    format!(
                                        "Error de red transitorio, reintentando en {}s (concurrencia {})",
                                        backoff_secs,
                                        active_concurrency
                                    )
                                },
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

                    if is_decode_error {
                        if let Some(source_html) = original_html_contents.get(&chapter_index) {
                            tx.send((chapter_index, source_html.to_string())).map_err(|_| {
                                format!(
                                    "Error traduciendo {} y no se pudo enviar fallback al writer: {}",
                                    file_name, err
                                )
                            })?;

                            retries.remove(&chapter_index);
                            decode_recovery_chapters.remove(&chapter_index);

                            translated_html_files += 1;
                            fallback_html_files += 1;
                            decode_fallback_chapters += 1;

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
                                        "Advertencia: fallback original en {} por respuestas invalidas de DeepSeek",
                                        file_name
                                    ),
                                    current_file: translated_html_files,
                                    total_files: html_total,
                                    percent,
                                    translated_characters,
                                },
                            );

                            if decode_fallback_chapters >= MAX_DECODE_ERROR_FALLBACK_CHAPTERS {
                                return Err(format!(
                                    "Se detuvo la traduccion para evitar consumo excesivo: DeepSeek devolvio respuestas invalidas en {} capitulos. Reintenta con menor concurrencia o mas tarde.",
                                    decode_fallback_chapters
                                ));
                            }

                            continue;
                        }

                        return Err(format!("Error traduciendo {}: {}", file_name, err));
                    }

                    if let Some(source_html) = original_html_contents.get(&chapter_index) {
                        tx.send((chapter_index, source_html.to_string())).map_err(|_| {
                            format!(
                                "Error traduciendo {} y no se pudo enviar fallback al writer: {}",
                                file_name, err
                            )
                        })?;

                        retries.remove(&chapter_index);
                        decode_recovery_chapters.remove(&chapter_index);

                        translated_html_files += 1;
                        fallback_html_files += 1;

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
                                    "Advertencia: fallback original en {} tras error de traduccion",
                                    file_name
                                ),
                                current_file: translated_html_files,
                                total_files: html_total,
                                percent,
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
        let (writer_finished, writer_fallbacks) = writer_thread
            .join()
            .map_err(|_| "Error en el worker de disco".to_string())??;
        writer = writer_finished;
        fallback_html_files += writer_fallbacks;
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

            let options = zip::write::FileOptions::default().compression_method(entry.compression());
            #[cfg(unix)]
            let options = if let Some(mode) = entry.unix_mode() {
                options.unix_permissions(mode)
            } else {
                options
            };

            if entry.is_dir() {
                (name, options, true, Vec::new())
            } else {
                let mut bytes = Vec::new();
                entry
                    .read_to_end(&mut bytes)
                    .map_err(|e| format!("Error leyendo archivo {}: {}", name, e))?;
                (name, options, false, bytes)
            }
        };

        if is_directory {
            writer
                .add_directory(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo crear directorio {}: {}", file_name, e))?;
        } else {
            writer
                .start_file(file_name.as_str(), options)
                .map_err(|e| format!("No se pudo crear archivo {} en ZIP: {}", file_name, e))?;
            writer
                .write_all(&bytes)
                .map_err(|e| format!("No se pudo escribir archivo {} en ZIP: {}", file_name, e))?;
        }
    }

    writer
        .finish()
        .map_err(|e| format!("No se pudo finalizar el EPUB de salida: {}", e))?;

    println!(
        "EPUBTR PERF: total {:.2}s (translated_files={}, translated_chars={}, fallback_files={})",
        overall_start.elapsed().as_secs_f32(),
        translated_html_files,
        translated_characters,
        fallback_html_files
    );

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

    notify_translation_completed_dialog(&app, overall_start.elapsed(), request.preview_only);

    Ok(TranslateEpubResult {
        output_path: absolute_output.to_string_lossy().to_string(),
        total_html_files,
        translated_html_files,
        translated_characters,
        preview_only: request.preview_only,
    })
}

fn build_file_options(meta: EntryMeta) -> zip::write::FileOptions {
    let options = zip::write::FileOptions::default().compression_method(meta.compression);
    #[cfg(unix)]
    let options = if let Some(mode) = meta.unix_mode {
        options.unix_permissions(mode)
    } else {
        options
    };
    options
}

fn is_rate_limit_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("429") || lower.contains("rate limit") || lower.contains("too many requests")
}

fn is_response_decode_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("deepseek_response_body_decode_error")
        || lower.contains("error decoding response body")
        || lower.contains("respuesta invalida de deepseek")
}

fn is_transient_transport_error(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("error de red")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection reset")
        || lower.contains("unexpected eof")
        || lower.contains("error de deepseek 500")
        || lower.contains("error de deepseek 502")
        || lower.contains("error de deepseek 503")
        || lower.contains("error de deepseek 504")
}

// Lee y mapea la estructura interna del EPUB ('META-INF/container.xml' y el OPF)
// para extraer el orden de lectura correcto de los capítulos y documentos (columna vertebral o "spine").
// Si el EPUB no declara el orden correctamente el parser fallará para intentar con ordenación por nombres.
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
            let full_path = join_epub_internal_path(base_path, href);
            reading_order.push(full_path);
        }
    }

    Ok(reading_order)
}

fn is_html_path(path: &str) -> bool {
    let lower = path.to_lowercase();
    lower.ends_with(".xhtml") || lower.ends_with(".html") || lower.ends_with(".htm")
}

fn make_absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }

    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    Ok(cwd.join(path))
}

fn join_epub_internal_path(base_path: &str, href: &str) -> String {
    let combined = if href.starts_with('/') {
        href.trim_start_matches('/').to_string()
    } else {
        format!("{}{}", base_path, href)
    };

    let mut segments: Vec<&str> = Vec::new();
    for segment in combined.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            let _ = segments.pop();
            continue;
        }
        segments.push(segment);
    }

    segments.join("/")
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

fn read_env_usize(key: &str, default: usize, min: usize, max: usize) -> usize {
    let Ok(raw) = env::var(key) else {
        return default.clamp(min, max);
    };

    match raw.trim().parse::<usize>() {
        Ok(value) => value.clamp(min, max),
        Err(_) => default.clamp(min, max),
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

fn notify_translation_completed_dialog(
    app: &tauri::AppHandle,
    elapsed: std::time::Duration,
    preview_only: bool,
) {
    let elapsed_text = format_elapsed_duration(elapsed);
    let task_label = if preview_only {
        "La vista previa"
    } else {
        "La traduccion"
    };

    app.dialog()
        .message(format!(
            "{} termino correctamente.\nTiempo total: {}",
            task_label, elapsed_text
        ))
        .title("Traduccion completada")
        .kind(MessageDialogKind::Info)
        .buttons(MessageDialogButtons::Ok)
        .show(|_| {});
}

fn format_elapsed_duration(elapsed: std::time::Duration) -> String {
    let total_seconds = elapsed.as_secs_f32();
    if total_seconds < 60.0 {
        format!("{:.1} segundos", total_seconds)
    } else {
        let minutes = (total_seconds / 60.0).floor() as u64;
        let seconds = total_seconds - (minutes as f32 * 60.0);
        format!("{} min {:.1} s", minutes, seconds)
    }
}

// Punto central para decidir cómo procesar una página HTML.
// O bien utiliza el 'modo de bloque completo' (enviando porciones largas integrales al modelo)
// o parsea iterativamente las etiquetas (tokeniza) y traduce de a poco (modo chunking)
// manteniendo intactas las etiquetas estructurales.
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
    if !options.enable_streaming && char_budget.is_none() && !options.force_text_node_mode {
        return translate_html_in_blocks(client, api_key, target_language, html, options).await;
    }

    let tokens = tokenize_html(html);
    let mut result = String::with_capacity(html.len());

    let mut consumed_characters = 0usize;
    let mut skip_tag_depth = 0usize;
    let mut budget_exhausted = false;
    let total_text_chars = count_translatable_text_chars(&tokens);
    // Ajusta el umbral de chunking para CJK: caracteres Han ≈ 1 token
    // en lugar de ≈ 4 (alfabeto latino). Así el chunking arranca a ~10K
    // tokens reales en ambos casos.
    let effective_chunk_threshold = if ai::is_mostly_cjk(html) {
        CHAPTER_TOKEN_THRESHOLD
    } else {
        options.chunk_threshold_chars
    };
    let enable_chunking = options.enable_chunking && total_text_chars > effective_chunk_threshold;
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
                let token_text = token.value;

                if skip_tag_depth > 0 || token_text.trim().is_empty() || budget_exhausted {
                    result.push_str(&token_text);
                    continue;
                }

                let remaining = char_budget.map(|limit| limit.saturating_sub(consumed_characters));
                let translated_piece = translate_text_preserving_whitespace(
                    client,
                    api_key,
                    target_language,
                    &token_text,
                    remaining,
                    options,
                    enable_chunking,
                    consumed_characters,
                    reporter,
                    total_text_chars,
                    progress_label.as_str(),
                )
                .await;

                let (translated_text, consumed, exhausted_now) = match translated_piece {
                    Ok(value) => value,
                    Err(err) => {
                        // In full mode, keep the original text node when one segment fails
                        // to avoid downgrading an entire chapter to fallback English.
                        if !options.enable_streaming && char_budget.is_none() {
                            println!(
                                "Warning: fallo segmentado, conservando texto original en nodo (error={})",
                                err
                            );
                            (token_text.clone(), 0, false)
                        } else {
                            return Err(err);
                        }
                    }
                };

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

// Traduce grandes pedazos de HTML interconectados (bloques).
// Esto reduce notablemente la cantidad de peticiones API realizadas.
// Descompone la página en partes más pequeñas configurables (full_block_min/target/max_chars),
// procesándolas en paralelo, re-ensamblándolas y gestionando bloqueos/rate-limits por la API.
async fn translate_html_in_blocks(
    client: &Client,
    api_key: &str,
    target_language: &str,
    html: &str,
    options: &TranslationChunkOptions,
) -> Result<(String, usize), String> {
    let tokens = tokenize_html(html);
    let consumed = count_translatable_text_chars(&tokens);
    if consumed == 0 {
        return Ok((html.to_string(), 0));
    }

    // Para texto CJK reduce los bloques proporcionalmente: cada Han ≈ 1 token,
    // mientras que el alfabeto latino ≈ 4 chars/token. Mantiene el mismo
    // presupuesto de tokens por bloque en ambos casos y evita truncamientos.
    // IMPORTANTE: los floors del .max() deben ser proporcionales al divisor CJK;
    // de lo contrario anulan la reducción y se siguen enviando bloques de 8000 chars
    // que generan TRUNCATED_BY_LENGTH al traducirse al español (expansión 2-4x).
    let block_divisor = if ai::is_mostly_cjk(html) { APPROX_CHARS_PER_TOKEN } else { 1 };
    let (min_floor, target_floor, max_floor) = if block_divisor > 1 {
        // CJK: cada char ≈ 1 token de entrada; la traducción al español expande 2-4x,
        // así que 2000 chars → ~4000-8000 tokens de salida (dentro del cap de 12288).
        (400, 800, 2_000)
    } else {
        (3_000, 5_000, 8_000)
    };
    let block_min    = (options.full_block_min_chars    / block_divisor).max(min_floor);
    let block_target = (options.full_block_target_chars / block_divisor).max(target_floor);
    let block_max    = (options.full_block_max_chars    / block_divisor).max(max_floor);

    let blocks = split_html_into_blocks(
        &tokens,
        block_min,
        block_target,
        block_max,
    );
    if blocks.is_empty() {
        return Ok((html.to_string(), consumed));
    }

    let mut fallback_blocks = 0usize;
    
    let semaphore = Arc::new(tokio::sync::Semaphore::new(3));
    let mut futures = FuturesUnordered::new();
    let num_blocks = blocks.len();

    for (i, block) in blocks.into_iter().enumerate() {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        block.as_str().hash(&mut hasher);
        let block_hash = hasher.finish();

        let cached_translation = BLOCK_TRANSLATION_CACHE
            .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
            .lock()
            .unwrap()
            .get(&block_hash)
            .cloned();

        let client_cloned = client.clone();
        let api_key_cloned = api_key.to_string();
        let target_lang_cloned = target_language.to_string();
        let semaphore_cloned = semaphore.clone();
        
        futures.push(async move {
            if let Some(cached) = cached_translation {
                return (i, block, Ok(cached));
            }

            let _permit = semaphore_cloned.acquire().await.unwrap();
            let result = translate_block_with_adaptive_splitting(
                &client_cloned,
                &api_key_cloned,
                options.provider.as_str(),
                options.model.as_str(),
                &target_lang_cloned,
                block.as_str(),
            ).await;

            if let Ok(ref text) = result {
                BLOCK_TRANSLATION_CACHE
                    .get_or_init(|| std::sync::Mutex::new(HashMap::new()))
                    .lock()
                    .unwrap()
                    .insert(block_hash, text.clone());
            }

            (i, block, result)
        });
    }

    let mut results = vec![String::new(); num_blocks];

    while let Some((i, original_block, result)) = futures.next().await {
        match result {
            Ok(chunk) => results[i] = chunk,
            Err(err) => {
                if is_rate_limit_error(&err)
                    || is_response_decode_error(&err)
                    || is_transient_transport_error(&err)
                {
                    return Err(err);
                }

                println!(
                    "Warning: fallo bloque y se conserva original para evitar truncado (error={})",
                    err
                );
                fallback_blocks += 1;
                results[i] = original_block;
            }
        }
    }

    let translated = results.join("");

    if fallback_blocks > 0 {
        println!(
            "Warning: se conservaron {} bloques originales para evitar truncado",
            fallback_blocks
        );
    }

    Ok((translated, consumed))
}

// Gestiona una retraducción adaptable ante errores "TRUNCATED_BY_LENGTH" (truncamiento).
// Cuando un bloque devuelve sólo una mitad traducida (porque DeepSeek superó su límite de tokens),
// este método subdivide el bloque en partes más pequeñas aún, hasta que el modelo devuelva todo correctamente.
// Si llegamos a nivel máximo de división adaptiva y sigue truncándose, retrocede al modo seguro "text node recovery".
async fn translate_block_with_adaptive_splitting(
    client: &Client,
    api_key: &str,
    provider: &str,
    model: &str,
    target_language: &str,
    block_html: &str,
) -> Result<String, String> {
    let mut pending_segments: VecDeque<(String, u8)> = VecDeque::new();
    pending_segments.push_back((block_html.to_string(), 0));
    let mut translated = String::with_capacity(block_html.len());

    while let Some((segment, depth)) = pending_segments.pop_front() {
        match ai::translate_text_with_retry(
            client,
            api_key,
            provider,
            model,
            target_language,
            segment.as_str(),
            MAX_RETRIES,
        )
        .await
        {
            Ok(piece) => translated.push_str(&piece),
            Err(err) => {
                if is_truncated_by_length_error(&err) && depth < ADAPTIVE_TRUNCATION_MAX_DEPTH {
                    let split_segments = split_segment_for_truncation_retry(&segment, depth);
                    if split_segments.len() > 1 {
                        println!(
                            "Warning: truncado por longitud, reduciendo tamano (nivel={}, partes={})",
                            depth + 1,
                            split_segments.len()
                        );

                        for split in split_segments.into_iter().rev() {
                            pending_segments.push_front((split, depth + 1));
                        }
                        continue;
                    }
                }

                if is_truncated_by_length_error(&err) {
                    println!(
                        "Warning: truncado persistente, activando recovery por nodos de texto"
                    );
                    let recovered =
                        translate_block_with_text_node_recovery(
                            client,
                            api_key,
                            provider,
                            model,
                            target_language,
                            segment.as_str(),
                        )
                        .await?;
                    translated.push_str(&recovered);
                    continue;
                }

                return Err(err);
            }
        }
    }

    Ok(translated)
}

fn split_segment_for_truncation_retry(segment_html: &str, depth: u8) -> Vec<String> {
    let segment_chars = segment_html.chars().count();
    let adaptive_min = ADAPTIVE_TRUNCATION_MIN_CHARS.min(RECOVERY_BLOCK_MIN_CHARS);

    if segment_chars < adaptive_min {
        return Vec::new();
    }

    let divisor = depth as usize + 2;
    let target = (segment_chars / divisor).clamp(adaptive_min, RECOVERY_BLOCK_TARGET_CHARS);
    let min = (target / 2).clamp(adaptive_min, target);
    let max = (target * 2).clamp(target, RECOVERY_BLOCK_MAX_CHARS);

    let tokens = tokenize_html(segment_html);
    split_html_into_blocks(&tokens, min, target, max)
}

fn is_truncated_by_length_error(error: &str) -> bool {
    error.contains("TRUNCATED_BY_LENGTH")
}

// Modalidad de extrema supervivencia que se lanza cuando el modo principal fracasó y las adaptaciones de truncaje fallaron.
// Tokeniza la fuente separando el HTML del texto y procesa individualmente solo las fracciones del texto libre detectado con Chunking,
// devolviendo un nuevo ensamble ultra detallado y blindando de errores de formato.
async fn translate_block_with_text_node_recovery(
    client: &Client,
    api_key: &str,
    provider: &str,
    model: &str,
    target_language: &str,
    block_html: &str,
) -> Result<String, String> {
    let recovery_options = TranslationChunkOptions {
        provider: provider.to_string(),
        model: model.to_string(),
        enable_chunking: true,
        chunk_threshold_chars: 1,
        chunk_min_chars: TRUNCATION_RECOVERY_CHUNK_MIN_CHARS,
        chunk_target_chars: TRUNCATION_RECOVERY_CHUNK_TARGET_CHARS,
        max_chunk_chars: TRUNCATION_RECOVERY_CHUNK_MAX_CHARS,
        enable_streaming: false,
        max_concurrent_requests: 1,
        dynamic_rate_limit: false,
        full_block_min_chars: RECOVERY_BLOCK_MIN_CHARS,
        full_block_target_chars: RECOVERY_BLOCK_TARGET_CHARS,
        full_block_max_chars: RECOVERY_BLOCK_MAX_CHARS,
        force_text_node_mode: true,
    };

    let tokens = tokenize_html(block_html);
    let total_text_chars = count_translatable_text_chars(&tokens);
    if total_text_chars == 0 {
        return Ok(block_html.to_string());
    }

    let mut translated = String::with_capacity(block_html.len());
    let mut skip_tag_depth = 0usize;
    let mut consumed_before = 0usize;

    for token in tokens {
        match token.kind {
            HtmlTokenKind::Tag => {
                update_skip_depth(&token.value, &mut skip_tag_depth);
                translated.push_str(&token.value);
            }
            HtmlTokenKind::Text => {
                if skip_tag_depth > 0 || token.value.trim().is_empty() {
                    translated.push_str(&token.value);
                    continue;
                }

                let (translated_text, consumed, _) = translate_text_preserving_whitespace(
                    client,
                    api_key,
                    target_language,
                    &token.value,
                    None,
                    &recovery_options,
                    true,
                    consumed_before,
                    None,
                    total_text_chars,
                    "Recuperando bloque truncado",
                )
                .await?;

                consumed_before += consumed;
                translated.push_str(&translated_text);
            }
        }
    }

    Ok(translated)
}

// Módulo especializado en la traducción de un hilo de texto sin descuidar ni mutar sus prefijos o sufijos 
// invisibles (espacios, enter, tabulaciones), indispensables para ciertas estructuraciones HTML.
// Segmenta internamente el contenido según oraciones naturales sí 'chunking' está activado
// o realiza un parseo lineal entero directo si el bloque no supera los límites fijados de tokens.
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
                        options.provider.as_str(),
                        options.model.as_str(),
                        target_language,
                        head,
                        options,
                        Some(&mut on_delta),
                    )
                    .await?
                } else {
                    translate_text(client, api_key, options.provider.as_str(), options.model.as_str(), target_language, head, options, None).await?
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
                        options.provider.as_str(),
                        options.model.as_str(),
                        target_language,
                        chunk,
                        options,
                        Some(&mut on_delta),
                    )
                    .await?
                } else {
                    translate_text(
                        client,
                        api_key,
                        options.provider.as_str(),
                        options.model.as_str(),
                        target_language,
                        chunk,
                        options,
                        None,
                    )
                    .await?
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
                options.provider.as_str(),
                options.model.as_str(),
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
                options.provider.as_str(),
                options.model.as_str(),
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
    provider: &str,
    model: &str,
    target_language: &str,
    text: &str,
    options: &TranslationChunkOptions,
    on_delta: Option<&mut (dyn FnMut(&str) + Send)>,
) -> Result<String, String> {
    if options.enable_streaming {
        ai::translate_text_with_retry_streaming(
            client,
            api_key,
            provider,
            model,
            target_language,
            text,
            MAX_RETRIES,
            on_delta,
        )
        .await
    } else {
        ai::translate_text_with_retry(client, api_key, provider, model, target_language, text, MAX_RETRIES).await
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

// Corta amigablemente el texto grande localizando oraciones completas y evitando romper su naturalidad semántica.
// Detecta puntos finales, signos de exclamación o de interrogación que terminen idealmente la locución 
// en las proximidades del target character predefinido.
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

            if matches!(ch, '.' | '!' | '?' | '\n' | '\u{3002}' | '\u{FF01}' | '\u{FF1F}') {
                // \u{3002}=。 \u{FF01}=！ \u{FF1F}=？ (puntuación CJK)
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

// Divide lógicamente el HTML de un archivo grande en secciones/bloques autónomos (generalmente correspondientes 
// con párrafos o divs enteros). Considera seguras las divisiones solo sí terminan en etiquetas de cierre naturales,
// salvo llegar a rebasar el umbral estricto por sobre "max_chars".
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

// Iterador simple pero eficaz que clasifica fragmentos de HTML entre dos tipos fijos:
// 'HtmlTokenKind::Tag': Porciones de texto encapsuladas en `<` y `>`.
// 'HtmlTokenKind::Text': Todo lo recidual externo a etiquetas.
// Evita el uso de Regex por rendimiento en miles de páginas renderizadas.
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
