# EPUBTR

Aplicacion de escritorio para traducir libros EPUB con IA, construida con Tauri (Rust) + React (TypeScript).

## Que hace el proyecto

- Abre un archivo `.epub`.
- Traduce el contenido HTML capitulo por capitulo usando DeepSeek.
- Conserva estructura EPUB (spine, imagenes, CSS y archivos no HTML).
- Escribe un EPUB de salida manteniendo el orden de lectura.

## Arquitectura general

### Frontend (React + TypeScript)

- UI principal: `src/App.tsx`
- Panel de traduccion: `src/components/TranslationPanel.tsx`
- Modal de API key: `src/navigation/components/ApiKeyModal.tsx`
- i18n: `src/i18n/config.ts`

Responsabilidades del frontend:

- Seleccionar archivo de entrada/salida.
- Gestionar API key.
- Invocar comandos Tauri.
- Mostrar progreso de traduccion en tiempo real.

### Backend (Rust + Tauri)

- Registro de comandos: `src-tauri/src/lib.rs`
- Pipeline EPUB: `src-tauri/src/translation.rs`
- Cliente IA (DeepSeek): `src-tauri/src/ai.rs`

Responsabilidades del backend:

- Leer y parsear EPUB (ZIP + OPF/spine).
- Ejecutar traduccion concurrente de HTML.
- Aplicar reintentos, control de truncado y fallbacks.
- Escribir EPUB final ordenado y completo.

## Tecnologias usadas

### Frontend

- React 19
- TypeScript 5
- Vite 7
- Tailwind CSS 4
- i18next + react-i18next
- Tauri JS API (`@tauri-apps/api`)
- Plugins Tauri frontend:
	- `@tauri-apps/plugin-dialog`
	- `@tauri-apps/plugin-store`

### Backend Rust

- `tauri` (comandos e integracion desktop)
- `tauri-plugin-dialog`
- `tauri-plugin-store`
- `reqwest` (cliente HTTP para DeepSeek)
- `tokio` (async y backoff)
- `futures-util` (pool de futures concurrentes)
- `zip` (lectura/escritura EPUB)
- `quick-xml` (parseo de container.xml y OPF/spine)
- `serde` / `serde_json` (serializacion)

## Librerias Rust clave y por que se usan

- `zip`: EPUB es un contenedor ZIP. Se usa para leer entradas y reconstruir salida.
- `quick-xml`: parseo robusto de XML EPUB (`container.xml` y `content.opf`).
- `reqwest`: llamadas HTTP a DeepSeek, incluyendo modo streaming/no-streaming.
- `tokio`: espera async, reintentos exponenciales y control de concurrencia.
- `futures-util`: `FuturesUnordered` para ejecutar varios capitulos en paralelo.

## Logica multihilo de traduccion EPUB

El flujo de libro completo usa un patron productor-consumidor:

1. **Productor async (traduccion)**
	 - Toma indices de capitulos HTML.
	 - Traduce en paralelo con concurrencia dinamica.
	 - Envia resultados al canal como `(indice, html_traducido)`.

2. **Consumidor dedicado (writer thread)**
	 - Hilo separado para escritura ZIP.
	 - Recibe resultados fuera de orden y los bufferiza.
	 - Escribe al ZIP solo cuando llega el `expected_index`.
	 - Garantiza orden correcto del EPUB de salida.

3. **Fallback seguro**
	 - Si un capitulo falla, se puede conservar HTML original para no truncar el archivo final.
	 - Si el canal se cierra inesperadamente, el writer mantiene completitud del EPUB.

## Logica de traduccion y control de calidad

- Deteccion de truncado por `finish_reason = "length"`.
- Division adaptativa de bloques HTML ante truncado.
- Recovery por nodos de texto para segmentos complejos.
- Validaciones de salida traducida:
	- no vacia,
	- sin bloques de codigo,
	- consistencia basica de etiquetas,
	- ratio de longitud razonable.

## Compatibilidad Windows y macOS

El proyecto esta orientado a funcionar en ambos sistemas en desarrollo y build local.

Puntos clave:

- Tauri v2 con `bundle.targets = "all"`.
- Iconos presentes para ambos sistemas:
	- `src-tauri/icons/icon.ico` (Windows)
	- `src-tauri/icons/icon.icns` (macOS)
- Manejo de rutas de entrada/salida en backend con validaciones de seguridad.

## Variables de entorno utiles

- `EPUBTR_MAX_CONCURRENCY`
- `EPUBTR_FULL_BLOCK_MIN_CHARS`
- `EPUBTR_FULL_BLOCK_TARGET_CHARS`
- `EPUBTR_FULL_BLOCK_MAX_CHARS`
- `EPUBTR_MAX_OUTPUT_TOKENS`

## Desarrollo local

### Requisitos

- Node.js 18+
- Rust toolchain estable
- Dependencias de Tauri segun tu OS

### Instalar dependencias

```bash
npm install
```

### Ejecutar en desarrollo

```bash
npm run tauri dev
```

### Build de produccion

```bash
npm run tauri build
```

### Verificacion backend Rust

```bash
cd src-tauri
cargo check
```

## Estructura resumida

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

- Actualmente el flujo principal es traduccion de libro completo.
- El pipeline prioriza robustez y completitud del EPUB antes que velocidad maxima agresiva.
- Las optimizaciones de rendimiento deben mantener:
	- orden de capitulos,
	- preservacion de estructura,
	- fallback seguro ante errores de red/modelo.
