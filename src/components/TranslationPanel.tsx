import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";
import type { AiConfig } from "../types/ai";

type TranslationProgressPayload = {
  status: string;
  message: string;
  currentFile: number;
  totalFiles: number;
  percent: number;
  translatedCharacters: number;
};

type TranslateResult = {
  outputPath: string;
  totalHtmlFiles: number;
  translatedHtmlFiles: number;
  translatedCharacters: number;
  previewOnly: boolean;
};

type LanguageOption = {
  code: string;
  label: string;
};

const LANGUAGE_OPTIONS: LanguageOption[] = [
  { code: "es", label: "Español" },
];

// Función auxiliar para generar la ruta de salida por defecto basada en el idioma de destino
function defaultOutputPath(inputPath: string, lang: string): string {
  const lower = inputPath.toLowerCase();
// Si el archivo de entrada no es un epub válido retorna un sufijo simple
  if (!lower.endsWith(".epub")) {
    return `${inputPath}_${lang}.epub`;
  }
// Devuelve la ruta eliminando la extensión y añadiendo el sufijo de idioma
  return `${inputPath.slice(0, -5)}_${lang}.epub`;
}

// Componente principal para el panel de traducción
export function TranslationPanel({
  aiConfig,
  hasApiKey,
  onOpenApiKey,
}: {
  aiConfig: AiConfig | null;
  hasApiKey: boolean;
  onOpenApiKey: () => void;
}) {
  const { t } = useTranslation();

// Estados locales para la ruta de entrada, salida y el idioma a traducir
  const [inputPath, setInputPath] = useState("");
  const [outputPath, setOutputPath] = useState("");
  const [targetLanguage, setTargetLanguage] = useState("es");
// Estados misceláneos de la IU
  const [isTranslating, setIsTranslating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const [progress, setProgress] = useState<TranslationProgressPayload>({
    status: "idle",
    message: "",
    currentFile: 0,
    totalFiles: 0,
    percent: 0,
    translatedCharacters: 0,
  });

  const translatedModeLabel = t("translation.fullModeHint");

// Abre un selector de archivos nativo para elegir el EPUB de entrada
  const pickInputEpub = async () => {
    const selected = await open({
      multiple: false,
      filters: [{ name: "EPUB", extensions: ["epub"] }],
    });

    if (typeof selected !== "string") {
      return;
    }

    setInputPath(selected);
    setOutputPath(defaultOutputPath(selected, targetLanguage));
    setError(null);
    setSuccessMessage(null);
  };

// Abre un selector de archivos nativo para guardar el archivo traducido
  const pickOutputEpub = async () => {
    const selected = await save({
      defaultPath: outputPath || defaultOutputPath(inputPath || "translated", targetLanguage),
      filters: [{ name: "EPUB", extensions: ["epub"] }],
    });

    if (!selected) {
      return;
    }

// Normaliza el nombre del archivo si no tiene extensión epub
    const normalized = selected.toLowerCase().endsWith(".epub")
      ? selected
      : `${selected}.epub`;

    setOutputPath(normalized);
    setError(null);
  };

// Inicia el proceso de traducción con el backend en Rust
  const startTranslation = async () => {
// Validación de las dependencias antes de empezar
    if (!hasApiKey) {
      setError(t("translation.errors.noApiKey"));
      return;
    }

    if (!inputPath) {
      setError(t("translation.errors.noInput"));
      return;
    }

    if (!outputPath) {
      setError(t("translation.errors.noOutput"));
      return;
    }

    setError(null);
    setSuccessMessage(null);
    setIsTranslating(true);
    let unlisten: (() => void) | null = null;

    try {
// Escucha el evento de progreso para mostrar el porcentaje en la IU
      unlisten = await listen<TranslationProgressPayload>(
        "translation-progress",
        (event) => {
          setProgress(event.payload);
        },
      );

// Invoca el comando al hilo principal (backend) pasando los datos de la tarea
      const result = await invoke<TranslateResult>("translate_epub", {
        request: {
          inputPath,
          outputPath,
          targetLanguage,
          previewOnly: false,
          apiKey: aiConfig?.apiKey ?? "",
          provider: aiConfig?.provider ?? "deepseek",
          model: aiConfig?.model ?? "deepseek-v4-pro",
        },
      });

// Al completarse, notifica en la IU el éxito del proceso
      setSuccessMessage(
        t("translation.success", {
          outputPath: result.outputPath,
          translatedFiles: result.translatedHtmlFiles,
          totalFiles: result.totalHtmlFiles,
        }),
      );
    } catch (translationError) {
// Despliega cualquier error proveniente del backend
      setError(String(translationError));
    } finally {
// Limpieza de estados y eventos
      unlisten?.();
      setIsTranslating(false);
    }
  };

  return (
    <div className="bg-white rounded-lg shadow-sm border border-slate-200 p-6 space-y-6">
      <div className="space-y-2">
        <h2 className="text-xl font-semibold text-slate-900">{t("translation.title")}</h2>
        <p className="text-sm text-slate-600">{t("translation.description")}</p>
      </div>

      <div className="grid grid-cols-1 md:grid-cols-2 gap-4">
        <div className="space-y-2">
          <label className="text-sm font-medium text-slate-700" htmlFor="target-language-select">
            {t("translation.targetLanguage")}
          </label>
          <select
            id="target-language-select"
            className="w-full border border-slate-300 rounded-md px-3 py-2 text-slate-900 bg-white"
            value={targetLanguage}
            onChange={(event) => {
              const nextLanguage = event.target.value;
              setTargetLanguage(nextLanguage);
              if (inputPath) {
                setOutputPath(defaultOutputPath(inputPath, nextLanguage));
              }
            }}
            disabled={isTranslating}
          >
            {LANGUAGE_OPTIONS.map((option) => (
              <option key={option.code} value={option.code}>
                {option.label}
              </option>
            ))}
          </select>
          <p className="text-xs text-slate-500">{t("translation.moreLanguagesSoon")}</p>
        </div>

        <div className="space-y-2">
          <span className="text-sm font-medium text-slate-700">{t("translation.mode")}</span>
          <div className="flex items-center gap-3">
            <button
              type="button"
              className="px-3 py-2 rounded-md text-sm border bg-blue-600 text-white border-blue-600 cursor-default"
              disabled
            >
              {t("translation.fullMode")}
            </button>
          </div>
          <p className="text-xs text-slate-500">{translatedModeLabel}</p>
        </div>
      </div>

      <div className="space-y-3">
        <div className="flex flex-wrap gap-3">
          <button
            type="button"
            onClick={pickInputEpub}
            className="px-4 py-2 bg-slate-900 text-white rounded-md hover:bg-slate-800 disabled:opacity-50"
            disabled={isTranslating}
          >
            {t("translation.pickInput")}
          </button>
          <button
            type="button"
            onClick={pickOutputEpub}
            className="px-4 py-2 border border-slate-300 text-slate-800 rounded-md hover:bg-slate-100 disabled:opacity-50"
            disabled={isTranslating || !inputPath}
          >
            {t("translation.pickOutput")}
          </button>
          {!hasApiKey && (
            <button
              type="button"
              onClick={onOpenApiKey}
              className="px-4 py-2 border border-amber-300 text-amber-700 rounded-md hover:bg-amber-50"
              disabled={isTranslating}
            >
              {t("translation.addApiKey")}
            </button>
          )}
        </div>

        <div className="text-sm text-slate-700">
          <p>
            <span className="font-medium">{t("translation.inputPath")}: </span>
            {inputPath || t("translation.noneSelected")}
          </p>
          <p>
            <span className="font-medium">{t("translation.outputPath")}: </span>
            {outputPath || t("translation.noneSelected")}
          </p>
        </div>
      </div>

      <div className="space-y-3">
        <button
          type="button"
          onClick={startTranslation}
          disabled={isTranslating}
          className="px-5 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700 disabled:opacity-50 inline-flex items-center gap-2"
        >
          {isTranslating && (
            <span className="inline-block h-4 w-4 rounded-full border-2 border-white border-b-transparent animate-spin" />
          )}
          {isTranslating ? t("translation.translating") : t("translation.start")}
        </button>

        <progress
          className="w-full h-3 [&::-webkit-progress-bar]:bg-slate-200 [&::-webkit-progress-bar]:rounded-full [&::-webkit-progress-value]:bg-blue-600 [&::-webkit-progress-value]:rounded-full [&::-moz-progress-bar]:bg-blue-600"
          value={Math.max(0, Math.min(progress.percent, 100))}
          max={100}
        />

        <div className="text-xs text-slate-600">
          {progress.message && <p>{progress.message}</p>}
          <p>
            {t("translation.progress")}: {progress.currentFile}/{progress.totalFiles} ({progress.percent.toFixed(1)}%)
          </p>
          <p>
            {t("translation.translatedChars")}: {progress.translatedCharacters}
          </p>
        </div>
      </div>

      {error && <p className="text-sm text-red-600">{error}</p>}
      {successMessage && <p className="text-sm text-green-700">{successMessage}</p>}
    </div>
  );
}
