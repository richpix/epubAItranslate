# AGENTS.md

## Dev commands

```bash
pnpm tauri dev      # full Tauri dev (frontend + backend, watches both)
pnpm tauri build    # production build
pnpm dev            # frontend only (Vite on :1420)
pnpm build          # frontend only (tsc && vite build)
cd src-tauri && cargo check   # Rust typecheck only
```

## Architecture

- **Frontend entry**: `src/App.tsx` (React 19, TypeScript, Tailwind CSS 4, Vite 7)
- **Backend entry**: `src-tauri/src/main.rs` → `epubtr_lib::run()` in `lib.rs`
- **Tauri commands**: `greet`, `ai::validate_api_key`, `translation::translate_epub` registered in `lib.rs`
- **Rust crates**: `src-tauri/src/ai.rs` (AI client), `src-tauri/src/translation.rs` (EPUB pipeline)
- **No monorepo**: single `package.json` + single `Cargo.toml`

## Toolchain quirks

- `pnpm` v11.1.1 is enforced via `packageManager` in `package.json`
- Vite dev server port is **1420** (fixed, not random)
- `src-tauri/**` is excluded from Vite's file watcher — Rust changes require restart
- Rust lib name is `epubtr_lib` (not `epubtr`) due to Windows cargo issue
- `tauri.conf.json` `build.beforeDevCommand` = `pnpm dev`, `build.beforeBuildCommand` = `pnpm build`

## Runtime env vars (override translation behavior)

- `EPUBTR_MAX_CONCURRENCY`
- `EPUBTR_FULL_BLOCK_MIN_CHARS`
- `EPUBTR_FULL_BLOCK_TARGET_CHARS`
- `EPUBTR_FULL_BLOCK_MAX_CHARS`
- `EPUBTR_MAX_OUTPUT_TOKENS`

## CI / Release

- Tags matching `v*` trigger builds on macOS (x64 + arm64), Windows, Ubuntu
- Uses `pnpm/action-setup@v4` with version `11.1.1`
- Node 22 for all CI jobs
- Linux build requires: `libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`

## Key dependencies

- `@tauri-apps/plugin-store` — persists API key locally
- `@tauri-apps/plugin-dialog` — file open/save dialogs
- `reqwest` with `stream` feature — SSE streaming for translation progress
- `zip` + `quick-xml` — EPUB read/write and OPF/spine parsing
- `tokio` + `futures-util` — async concurrency and chapter parallelism
