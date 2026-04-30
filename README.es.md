[**English**](README.md) | **Español**

# EPUBTR

[![Tauri](https://img.shields.io/badge/Tauri-2.0-FFC131?style=flat-square&logo=tauri&logoColor=white)](https://tauri.app)
[![React](https://img.shields.io/badge/React-19-61DAFB?style=flat-square&logo=react)](https://react.dev)
[![TypeScript](https://img.shields.io/badge/TypeScript-5-3178C6?style=flat-square&logo=typescript&logoColor=white)](https://www.typescriptlang.org)
[![Rust](https://img.shields.io/badge/Rust-1.77+-DEA584?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Vite](https://img.shields.io/badge/Vite-7-646CFF?style=flat-square&logo=vite&logoColor=white)](https://vitejs.dev)
[![Tailwind CSS](https://img.shields.io/badge/Tailwind_CSS-4-38B2AC?style=flat-square&logo=tailwind-css&logoColor=white)](https://tailwindcss.com)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue?style=flat-square)](./LICENSE)

Aplicación de escritorio para traducir libros EPUB usando IA (Gemini, OpenAI, Deepseek), construida con **Tauri (Rust)** + **React (TypeScript)**.

## Qué hace el proyecto

- Abre un archivo `.epub`.
- Traduce el contenido HTML capítulo por capítulo a través de **DeepSeek**, **Gemini**, **OpenAI**.
- Preserva la estructura del EPUB (spine, imágenes, CSS y archivos no HTML).
- Genera un EPUB completamente legible manteniendo el orden de lectura original.

## Arquitectura general

### Frontend (React + TypeScript)

- UI principal: `src/App.tsx`
- Panel de traducción: `src/components/TranslationPanel.tsx`
- Modal de API Key: `src/navigation/components/ApiKeyModal.tsx`
- i18n: `src/i18n/config.ts`

Responsabilidades:
- Seleccionar archivo de entrada/salida.
- Gestionar la clave de API.
- Invocar comandos de Tauri.
- Mostrar progreso de traducción en tiempo real.

### Backend (Rust + Tauri)

- Registro de comandos: `src-tauri/src/lib.rs`
- Pipeline EPUB: `src-tauri/src/translation.rs`
- Cliente IA (DeepSeek, Gemini, OpenAI): `src-tauri/src/ai.rs`

Responsabilidades:
- Leer y parsear EPUB (ZIP + OPF/spine).
- Ejecutar traducción HTML concurrente.
- Manejar reintentos, control de truncamiento y fallbacks.
- Escribir el EPUB final ordenado y completo.

## Tecnologías utilizadas

### Frontend

- React 19
- TypeScript 5
- Vite 7
- Tailwind CSS 4
- i18next + react‑i18next
- Tauri JS API (`@tauri-apps/api`)
- Plugins Tauri:
  - `@tauri-apps/plugin-dialog`
  - `@tauri-apps/plugin-store`

### Backend Rust

- `tauri` – comandos e integración de escritorio
- `tauri-plugin-dialog`
- `tauri-plugin-store`
- `reqwest` – cliente HTTP para las APIs de IA
- `tokio` – runtime asíncrono, backoff, concurrencia
- `futures-util` – pool de futuros concurrentes
- `zip` – lectura/escritura de EPUB (contenedor ZIP)
- `quick-xml` – parseo de `container.xml` y OPF/spine
- `serde` / `serde_json` – serialización

## Aspectos destacados del código Rust

### Concurrencia y paralelismo

- **Pool de futuros asíncronos** con `FuturesUnordered` — decenas de capítulos se traducen concurrentemente sobre el runtime tokio sin crear hilos del sistema.
- **Control de congestión AIMD adaptativo** — la concurrencia se auto-escala de 6 a 10 según la tasa de éxito; aumenta en rachas de éxito y disminuye en errores, imitando el control de congestión TCP.
- **Paralelismo con semáforo** — dentro de un mismo capítulo, hasta 3 bloques HTML se traducen en paralelo usando `tokio::sync::Semaphore(3)`, evitando sobrecargar la API mientras se maximiza el rendimiento.
- **Arquitectura productor‑consumidor** — un productor asíncrono (tokio) envía capítulos traducidos a través de un canal `mpsc` a un **hilo del sistema dedicado** que escribe el archivo ZIP, manteniendo la crate síncrona `zip` fuera del runtime asíncrono.
- **Buffer de reordenamiento** — el hilo escritor mantiene un `HashMap<usize, String>` para reensamblar los resultados fuera de orden, escribiendo al ZIP solo cuando llega el `expected_index`.

### Resiliencia y reintentos

- **Doble capa de reintentos**: backoff exponencial a nivel de IA (duplicando la espera hasta 10 s) + backoff exponencial a nivel de capítulo con **jitter determinista** (basado en hash del índice del capítulo) para evitar el efecto thundering herd en reintentos masivos.
- **Clasificación de errores en tres niveles**: no reintentable (truncamiento, errores de decodificación), límite de tasa (429), transporte transitorio (timeout, 5xx) — cada uno con una ruta de recuperación diferente.
- **Cadena de degradación gradual**: modo bloque completo → división adaptativa (hasta 3 niveles) → recuperación por nodos de texto → conservar HTML original (garantiza la integridad del EPUB).

### Caché

- **Caché de system prompt** (`OnceLock<Mutex<HashMap>>`) — los prompts de sistema se generan una vez por idioma destino y se reutilizan en todas las llamadas API.
- **Caché de bloques traducidos** (`OnceLock<Mutex<HashMap<u64, String>>>`) — bloques HTML idénticos (ej. encabezados/pies de página repetidos) se traducen una sola vez y se reutilizan mediante búsqueda por hash.

### HTTP y redes

- **Connection pooling** — `reqwest::Client` con `pool_max_idle_per_host(64)` y `tcp_nodelay(true)` reutiliza conexiones en todas las peticiones.
- **Streaming SSE** — parseo de Server‑Sent Events con callbacks `on_delta` en tiempo real para progreso carácter por carácter en modo vista previa.

### Procesamiento HTML

- **Tokenizador O(n) personalizado** — tokenizador HTML de escaneo lineal (sin regex) para rendimiento en archivos grandes.
- **División de bloques consciente de HTML** — divide en límites de párrafo (`</p>`, `</div>`, `</section>`, etc.) para preservar el contexto estructural.
- **División de texto consciente de oraciones** — divide en `.`, `!`, `?`, `\n` y puntuación CJK para fragmentación más natural.
- **Ajuste CJK** — los tamaños de bloque se dividen por `APPROX_CHARS_PER_TOKEN` (4) cuando se detectan caracteres Han, equiparando la densidad del tokenizador.

### Memoria y seguridad

- **`Arc<str>`** para contenido HTML compartido — evita clonar strings grandes.
- **Pre‑asignación con `String::with_capacity`** en todo el pipeline.
- **Guard de concurrencia `AtomicBool`** con `compare_exchange` + guard RAII previene múltiples traducciones simultáneas.
- **Ajuste por variables de entorno** — todos los parámetros críticos (concurrencia, tamaños de bloque, tokens de salida) son sobreescribibles en tiempo de ejecución.

## Lógica de traducción EPUB multihilo

El flujo de libro completo usa el patrón **productor‑consumidor** descrito arriba:

1. **Productor asíncrono (traducción)** — toma los índices de los capítulos HTML, traduce en paralelo con concurrencia dinámica, envía los resultados a un canal como `(index, translated_html)`.

2. **Consumidor dedicado (hilo escritor)** — un hilo del sistema separado maneja la escritura del ZIP, recibe resultados fuera de orden y los bufferiza, escribe al ZIP solo cuando llega el `expected_index`, garantizando el orden correcto de salida.

3. **Fallback seguro** — si un capítulo falla, se conserva el HTML original; si el canal se cierra inesperadamente, el escritor aún produce un EPUB completo.

## Lógica de traducción y control de calidad

- Detección de truncamiento vía `finish_reason = "length"`.
- División adaptativa de bloques HTML ante truncamiento.
- Recuperación por nodos de texto para segmentos complejos.
- Validaciones de la salida traducida:
  - no vacía,
  - sin bloques de código,
  - consistencia básica de etiquetas,
  - relación de longitud razonable.

## Compatibilidad con Windows y macOS

El proyecto está diseñado para ejecutarse en ambos sistemas durante desarrollo y compilación local.

Puntos clave:

- Tauri v2 con `bundle.targets = "all"`.
- Iconos presentes para ambas plataformas:
  - `src-tauri/icons/icon.ico` (Windows)
  - `src-tauri/icons/icon.icns` (macOS)
- Manejo de rutas en el backend con validaciones de seguridad.

## Uso de la clave API

La aplicación requiere una clave de API de uno de los proveedores de IA soportados (DeepSeek, Gemini, OpenAI) para realizar traducciones.

### Cómo funciona

1. **Ingresa tu clave** – Abre la barra lateral y haz clic en el botón **AI Key** para abrir el modal de configuración.
2. **Validación** – Antes de guardar, la clave se valida haciendo una petición de prueba a la API del proveedor seleccionado. Solo las claves válidas se persisten.
3. **Almacenamiento** – La clave API se almacena **localmente en tu máquina** usando [Tauri plugin-store](https://v2.tauri.app/plugin/store/), que la guarda en un archivo de configuración seguro a nivel de sistema (`.config.dat`). La clave **nunca se envía a ningún servidor que no sea el proveedor de IA que seleccionaste**, y **nunca se sube, registra o almacena en ningún servidor remoto** por esta aplicación.
4. **Eliminar** – Puedes eliminar la clave almacenada en cualquier momento desde el mismo modal.

> **Garantía de privacidad:** Tu clave API permanece en tu dispositivo. Solo se usa para autenticar peticiones directamente al proveedor de IA que elijas (DeepSeek, Google Gemini u OpenAI). Ningún tercero tiene acceso a ella.

## Variables de entorno útiles

- `EPUBTR_MAX_CONCURRENCY`
- `EPUBTR_FULL_BLOCK_MIN_CHARS`
- `EPUBTR_FULL_BLOCK_TARGET_CHARS`
- `EPUBTR_FULL_BLOCK_MAX_CHARS`
- `EPUBTR_MAX_OUTPUT_TOKENS`

## Desarrollo local

### Prerrequisitos

- Node.js 18+
- Rust toolchain estable
- Dependencias de sistema de Tauri (ver [documentación de Tauri](https://tauri.app/start/prerequisites/))

### Instalar dependencias

```bash
npm install
```

### Ejecutar en modo desarrollo

```bash
npm run tauri dev
```

### Compilación para producción

```bash
npm run tauri build
```

### Verificación del backend Rust

```bash
cd src-tauri
cargo check
```

## Estructura del proyecto (simplificada)
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

## Notas operativas

- El flujo principal es actualmente la traducción de libro completo.
- El pipeline prioriza la completitud y robustez del EPUB sobre la velocidad agresiva.
- Las optimizaciones de rendimiento deben preservar:
  el orden de los capítulos,
  la integridad estructural,
  el fallback seguro ante errores de red/modelo.
