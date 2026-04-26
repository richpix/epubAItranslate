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

## Multithreaded EPUB translation logic

The full‑book flow uses a **producer‑consumer** pattern:

1. **Async producer (translation)**
   - Takes indices of HTML chapters.
   - Translates in parallel with dynamic concurrency.
   - Sends results to a channel as `(index, translated_html)`.

2. **Dedicated consumer (writer thread)**
   - A separate thread handles ZIP writing.
   - Receives out‑of‑order results and buffers them.
   - Writes to the ZIP only when the `expected_index` arrives.
   - Guarantees the correct order in the output EPUB.

3. **Safe fallback**
   - If a chapter fails, the original HTML can be kept to avoid truncation.
   - If the channel closes unexpectedly, the writer still produces a complete EPUB.

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
