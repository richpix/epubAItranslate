import sys

with open('src/translation.rs', 'r', encoding='utf-8') as f:
    text = f.read()

start_marker = '} else {'
end_marker = '    // Fase 2: Copiar el resto de los archivos'

start_idx = text.find(start_marker, text.find('if request.preview_only {'))
end_idx = text.find(end_marker, start_idx)

if start_idx == -1 or end_idx == -1:
    print('Markers not found')
    sys.exit(1)

new_else = '''} else {
        let html_indices = spine_entries
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                matches!(entry.content, SpineEntryContent::Html(_)).then_some(index)
            })
            .collect::<Vec<_>>();
        let html_total = html_indices.len();

        let (tx, rx) = std::sync::mpsc::channel::<(usize, String)>();
        
        // --- Dedicated System I/O Worker (Writer Thread) ---
        let mut writer_local = writer;
        let mut expected_index = 0usize;
        let mut buffered_html: HashMap<usize, String> = HashMap::new();
        
        let mut original_html_contents = HashMap::new();
        let mut original_file_names = HashMap::new();
        for (idx, entry) in spine_entries.iter().enumerate() {
            if let SpineEntryContent::Html(ref content) = entry.content {
                original_html_contents.insert(idx, content.clone());
                original_file_names.insert(idx, entry.file_name.clone());
            }
        }
        
        let writer_thread = std::thread::spawn(move || -> Result<ZipWriter<File>, String> {
            while expected_index < spine_entries.len() {
                let entry = &spine_entries[expected_index];
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

        // --- Async Pipeline for Translation ---
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
                                "Traduciendo en paralelo {} ({} con.)",
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
                                    "Rate limit, esperando {}s. ({} con.)",
                                    backoff_secs, active_concurrency
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
        writer = writer_thread.join().map_err(|_| "Fallo I/O thread".to_string())??;
    }

'''
new_text = text[:start_idx] + new_else + text[end_idx:]
with open('src/translation.rs', 'w', encoding='utf-8') as f:
    f.write(new_text)

