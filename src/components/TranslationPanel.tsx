import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useTranslation } from "react-i18next";

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

function defaultOutputPath(inputPath: string, lang: string): string {
  const lower = inputPath.toLowerCase();
  if (!lower.endsWith(".epub")) {
    return `${inputPath}_${lang}.epub`;
  }
  return `${inputPath.slice(0, -5)}_${lang}.epub`;
}

export function TranslationPanel({
  apiKey,
  hasApiKey,
  onOpenApiKey,
}: {
  apiKey: string | null;
  hasApiKey: boolean;
  onOpenApiKey: () => void;
}) {
  const { t } = useTranslation();

  const [inputPath, setInputPath] = useState("");
  const [outputPath, setOutputPath] = useState("");
  const [targetLanguage, setTargetLanguage] = useState("es");
  const previewOnly = true;
  const fullModeEnabled = false;
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

  const translatedModeLabel = t("translation.fullModeDisabled");

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

  const pickOutputEpub = async () => {
    const selected = await save({
      defaultPath: outputPath || defaultOutputPath(inputPath || "translated", targetLanguage),
      filters: [{ name: "EPUB", extensions: ["epub"] }],
    });

    if (!selected) {
      return;
    }

    const normalized = selected.toLowerCase().endsWith(".epub")
      ? selected
      : `${selected}.epub`;

    setOutputPath(normalized);
    setError(null);
  };

  const startTranslation = async () => {
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

    const unlisten = await listen<TranslationProgressPayload>(
      "translation-progress",
      (event) => {
        setProgress(event.payload);
      },
    );

    try {
      const result = await invoke<TranslateResult>("translate_epub", {
        request: {
          inputPath,
          outputPath,
          targetLanguage,
          previewOnly,
          previewPages: 5,
          apiKey: apiKey ?? "",
        },
      });

      setSuccessMessage(
        t("translation.success", {
          outputPath: result.outputPath,
          translatedFiles: result.translatedHtmlFiles,
          totalFiles: result.totalHtmlFiles,
        }),
      );
    } catch (translationError) {
      setError(String(translationError));
    } finally {
      unlisten();
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
              onClick={() => undefined}
              className={`px-3 py-2 rounded-md text-sm border ${
                previewOnly
                  ? "bg-blue-600 text-white border-blue-600"
                  : "bg-white text-slate-700 border-slate-300"
              }`}
              disabled={isTranslating}
            >
              {t("translation.previewMode")}
            </button>
            <button
              type="button"
              onClick={() => undefined}
              className="px-3 py-2 rounded-md text-sm border bg-slate-100 text-slate-400 border-slate-200 cursor-not-allowed"
              disabled={isTranslating || !fullModeEnabled}
              title={t("translation.fullModeDisabled")}
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
