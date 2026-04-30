**English** | [**Español**](README.es.md)

# EPUBTR

[![Tauri](https://img.shields.io/badge/Tauri-2.0-FFC131?style=flat-square&logo=tauri&logoColor=white)](https://tauri.app)
[![React](https://img.shields.io/badge/React-19-61DAFB?style=flat-square&logo=react)](https://react.dev)
[![TypeScript](https://img.shields.io/badge/TypeScript-5-3178C6?style=flat-square&logo=typescript&logoColor=white)](https://www.typescriptlang.org)
[![Rust](https://img.shields.io/badge/Rust-1.77+-DEA584?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Vite](https://img.shields.io/badge/Vite-7-646CFF?style=flat-square&logo=vite&logoColor=white)](https://vitejs.dev)
[![Tailwind CSS](https://img.shields.io/badge/Tailwind_CSS-4-38B2AC?style=flat-square&logo=tailwind-css&logoColor=white)](https://tailwindcss.com)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=flat-square)](./LICENSE)

Desktop application to translate EPUB books using AI (Gemini, OpenAI, Deepseek), built with **Tauri (Rust)** + **React (TypeScript)**.

## What the project does

- Opens an `.epub` file.
- Translates the HTML content chapter by chapter via **DeepSeek**, **Gemini**, **OpenAI** .
- Preserves the EPUB structure (spine, images, CSS, and non‑HTML files).
- Outputs a fully readable EPUB while keeping the original reading order.

## General architecture

### Frontend (React + TypeScript)

- Main UI: `src/App.tsx`
- Translation panel: `src/components/TranslationPanel.tsx`
- API key modal: `src/navigation/components/ApiKeyModal.tsx`
- i18n: `src/i18n/config.ts`

Responsibilities:
- Select input/output file.
- Manage API key.
- Invoke Tauri commands.
- Show real‑time translation progress.

### Backend (Rust + Tauri)

- Command registration: `src-tauri/src/lib.rs`
- EPUB pipeline: `src-tauri/src/translation.rs`
- AI client (DeepSeek): `src-tauri/src/ai.rs`

Responsibilities:
- Read and parse EPUB (ZIP + OPF/spine).
- Run concurrent HTML translation.
- Handle retries, truncation control, and fallbacks.
- Write the final ordered, complete EPUB.

## Technologies used

### Frontend

- React 19
- TypeScript 5
- Vite 7
- Tailwind CSS 4
- i18next + react‑i18next
- Tauri JS API (`@tauri-apps/api`)
- Tauri plugins:
  - `@tauri-apps/plugin-dialog`
  - `@tauri-apps/plugin-store`

### Rust Backend

- `tauri` – commands and desktop integration
- `tauri-plugin-dialog`
- `tauri-plugin-store`
- `reqwest` – HTTP client for DeepSeek
- `tokio` – async runtime, backoff, concurrency
- `futures-util` – concurrent future pool
- `zip` – EPUB read/write (ZIP container)
- `quick-xml` – parsing `container.xml` and OPF/spine
- `serde` / `serde_json` – serialization

## Key Rust libraries and why

- **`zip`** – EPUB is a ZIP container; used to read entries and rebuild the output.
- **`quick-xml`** – robust XML parser for `container.xml` and `content.opf`.
- **`reqwest`** – HTTP calls to DeepSeek, both streaming and non‑streaming modes.
- **`tokio`** – async waiting, exponential retries, concurrency control.
- **`futures-util`** – `FuturesUnordered` for parallel chapter translation.

## Rust code highlights

### Concurrency & parallelism

- **Async future pool** with `FuturesUnordered` — dozens of chapter translations run concurrently on the tokio runtime without spawning OS threads.
- **Adaptive AIMD congestion control** — concurrency auto-scales from 6 up to 10 based on success rate; it increases on success streaks and decreases on errors, mimicking TCP congestion control.
- **Semaphore‑gated parallelism** — within a single chapter, up to 3 HTML blocks are translated in parallel using a `tokio::sync::Semaphore(3)`, preventing API overload while maximizing throughput.
- **Producer‑consumer architecture** — an async tokio producer sends translated chapters through an `mpsc` channel to a **dedicated OS thread** that writes the ZIP file, keeping the synchronous `zip` crate off the async runtime.
- **Reorder buffer** — the writer thread holds a `HashMap<usize, String>` to reassemble out‑of‑order results, writing to the ZIP only when `expected_index` arrives.

### Resilience & retries

- **Two‑layer retry**: AI‑level exponential backoff (doubling delay up to 10 s) + chapter‑level exponential backoff with **deterministic jitter** (hash‑based per chapter index) to prevent thundering herd on mass retries.
- **Three‑tier error classification**: non‑retryable (truncation, decode errors), rate‑limit (429), transient transport (timeout, 5xx) — each with a different recovery path.
- **Graceful degradation chain**: full‑block mode → adaptive splitting (up to 3 levels) → text‑node recovery → keep original HTML (guarantees EPUB integrity).

### Caching

- **System prompt cache** (`OnceLock<Mutex<HashMap>>`) — system prompts are generated once per target language and reused across all API calls.
- **Block translation cache** (`OnceLock<Mutex<HashMap<u64, String>>>`) — identical HTML blocks (e.g. repeated headers/footers) are translated only once and reused via hash lookup.

### HTTP & networking

- **Connection pooling** — `reqwest::Client` with `pool_max_idle_per_host(64)` and `tcp_nodelay(true)` reuses connections across all requests.
- **SSE streaming** — Server‑Sent Events parsing with real‑time `on_delta` callbacks for character‑by‑character progress in preview mode.

### HTML processing

- **Custom O(n) tokenizer** — linear‑scan HTML tokenizer (no regex) for performance on large files.
- **HTML‑aware block splitting** — splits at paragraph boundaries (`</p>`, `</div>`, `</section>`, etc.) to preserve structural context.
- **Sentence‑aware text splitting** — splits at `.`, `!`, `?`, `\n`, and CJK punctuation boundaries for more natural chunking.
- **CJK‑aware sizing** — block sizes are divided by `APPROX_CHARS_PER_TOKEN` (4) when Han characters are detected, matching tokenizer density.

### Memory & safety

- **`Arc<str>`** for shared HTML content — avoids cloning large strings.
- **`String::with_capacity`** pre‑allocation throughout the pipeline.
- **`AtomicBool` concurrency guard** with `compare_exchange` + RAII guard prevents multiple simultaneous translations.
- **Environment variable tuning** — all critical parameters (concurrency, block sizes, output tokens) are overridable at runtime.

## Multithreaded EPUB translation logic

The full‑book flow uses the **producer‑consumer** pattern described above:

1. **Async producer (translation)** — takes indices of HTML chapters, translates in parallel with dynamic concurrency, sends results to a channel as `(index, translated_html)`.

2. **Dedicated consumer (writer thread)** — a separate OS thread handles ZIP writing, receives out‑of‑order results and buffers them, writes to the ZIP only when the `expected_index` arrives, guaranteeing correct output order.

3. **Safe fallback** — if a chapter fails, the original HTML is kept; if the channel closes unexpectedly, the writer still produces a complete EPUB.

## Translation logic and quality control

- Truncation detection via `finish_reason = "length"`.
- Adaptive HTML block splitting on truncation.
- Text‑node recovery for complex segments.
- Translated output validations:
  - not empty,
  - no code blocks,
  - basic tag consistency,
  - reasonable length ratio.

## Windows and macOS compatibility

The project is designed to run on both systems during development and local builds.

Key points:

- Tauri v2 with `bundle.targets = "all"`.
- Icons present for both platforms:
  - `src-tauri/icons/icon.ico` (Windows)
  - `src-tauri/icons/icon.icns` (macOS)
- Path handling in the backend with security validations.

## API key usage

The application requires an API key from one of the supported AI providers (DeepSeek, Gemini, or OpenAI) to perform translations.

### How it works

1. **Enter your key** – Open the sidebar and click the **AI Key** button to open the configuration modal.
2. **Validation** – Before saving, the key is validated by making a test request to the selected provider's API. Only valid keys are persisted.
3. **Storage** – The API key is stored **locally on your machine** using [Tauri's plugin-store](https://v2.tauri.app/plugin/store/), which saves it to an OS-level secure configuration file (`.config.dat`). The key is **never sent to any server other than the AI provider you selected**, and it is **never uploaded, logged, or stored on any remote server** by this application.
4. **Clear** – You can remove the stored key at any time from the same modal.

> **Privacy guarantee:** Your API key stays on your device. It is only used to authenticate requests directly to the AI provider you choose (DeepSeek, Google Gemini, or OpenAI). No third party has access to it.

## Useful environment variables

- `EPUBTR_MAX_CONCURRENCY`
- `EPUBTR_FULL_BLOCK_MIN_CHARS`
- `EPUBTR_FULL_BLOCK_TARGET_CHARS`
- `EPUBTR_FULL_BLOCK_MAX_CHARS`
- `EPUBTR_MAX_OUTPUT_TOKENS`

## Local development

### Prerequisites

- Node.js 18+
- Stable Rust toolchain
- Tauri system dependencies (see [Tauri docs](https://tauri.app/start/prerequisites/))

### Install dependencies

```bash
npm install
```

### Run in development mode

```bash
npm run tauri dev
```

### Production build

```bash
npm run tauri build
```

### Backend Rust check

```bash
cd src-tauri
cargo check
```

## Project structure (simplified)
```text
src/
	App.tsx
	components/TranslationPanel.tsx
	navigation/components/ApiKeyModal.tsx
	i18n/

src-tauri/
	src/lib.rs
	src/translation.rs
	src/ai.rs
	tauri.conf.json
```

## Operational notes

-The main flow is currently full‑book translation.

-The pipeline prioritizes EPUB completeness and robustness over aggressive speed.

-Performance optimizations must preserve:
 chapter order,
 structural integrity,
 safe fallback on network/model errors.
