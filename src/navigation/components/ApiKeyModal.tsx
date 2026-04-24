import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import {
  DEEPSEEK_MODELS,
  DEFAULT_AI_CONFIG,
  GEMINI_MODEL,
  OPENAI_MODEL,
  type AiConfig,
  type AiProvider,
} from "../../types/ai";
import {
  clearAiConfigInStore,
  loadAiConfigFromStore,
  saveAiConfigToStore,
} from "../../utils/aiConfig";

// Componente modal para gestionar la persistencia y lectura de la API Key
export function ApiKeyModal({
  isOpen,
  onClose,
  onApiKeyChanged,
}: {
  isOpen: boolean;
  onClose: () => void;
  onApiKeyChanged?: () => void;
}) {
  const { t } = useTranslation();
  const [provider, setProvider] = useState<AiProvider>(DEFAULT_AI_CONFIG.provider);
  const [apiKey, setApiKey] = useState(DEFAULT_AI_CONFIG.apiKey);
  const [model, setModel] = useState<AiConfig["model"]>(DEFAULT_AI_CONFIG.model);
  const [isValidating, setIsValidating] = useState(false);
  const [error, setError] = useState<string | null>(null);

// Carga la clave API almacenada cuando el modal se abre
  useEffect(() => {
    if (isOpen) {
      loadStore();
    }
  }, [isOpen]);

// Función asincrónica para obtener la API key desde el almacenamiento tauri-store
  const loadStore = async () => {
    try {
      const config = await loadAiConfigFromStore();
      if (!config) return;
      setProvider(config.provider);
      setApiKey(config.apiKey);
      setModel(config.model);
    } catch (err) {
      console.error("Failed to load store", err);
    }
  };

  const effectiveModel = provider === "deepseek"
    ? model
    : provider === "gemini"
      ? GEMINI_MODEL
      : OPENAI_MODEL;

// Procesa el guardado en disco si la API key es válida
  const handleSave = async () => {
// Evita claves en blanco o compuestas solo por espacios
    if (!apiKey.trim()) {
      setError(t("modal.errorEmpty"));
      return;
    }
    setIsValidating(true);
    setError(null);

    try {
// Llama al backend en Rust para la validación antes de conservar permanentemente el valor
      const isValid = await invoke<boolean>("validate_api_key", {
        provider,
        apiKey,
        model: effectiveModel,
      });
      if (isValid) {
        await saveAiConfigToStore({
          provider,
          apiKey,
          model: effectiveModel,
        });
        onApiKeyChanged?.();
        onClose();
      } else {
        setError(t("modal.errorInvalid"));
      }
    } catch (err: any) {
// Despliega error local si falló el guardado o validación
      setError(err.toString());
    } finally {
      setIsValidating(false);
    }
  };

// Maneja el borrado de la clave almacenada
  const handleDelete = async () => {
    try {
      await clearAiConfigInStore();
      setProvider(DEFAULT_AI_CONFIG.provider);
      setApiKey(DEFAULT_AI_CONFIG.apiKey);
      setModel(DEFAULT_AI_CONFIG.model);
      onApiKeyChanged?.();
      onClose();
    } catch (err) {
      console.error("Failed to delete key", err);
    }
  };

// No renderiza nada si el estado isOpen es falso
  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-white p-6 rounded-lg shadow-xl w-96 max-w-[90vw]">
        <h2 className="text-xl font-bold mb-4 text-slate-900">
          {t("modal.apiKeyTitle")}
        </h2>
        <label className="block text-sm font-medium text-slate-700 mb-1">
          {t("modal.providerLabel")}
        </label>
        <select
          className="w-full p-2 border border-slate-300 rounded mb-3 focus:outline-none focus:ring-2 focus:ring-blue-500 text-slate-900"
          value={provider}
          onChange={(e) => {
            const nextProvider = e.target.value as AiProvider;
            setProvider(nextProvider);
            if (nextProvider === "deepseek" && !DEEPSEEK_MODELS.includes(model as (typeof DEEPSEEK_MODELS)[number])) {
              setModel(DEEPSEEK_MODELS[0]);
            }
          }}
          disabled={isValidating}
        >
          <option value="deepseek">DeepSeek</option>
          <option value="gemini">Gemini</option>
          <option value="openai">OpenAI</option>
        </select>
        {provider === "deepseek" ? (
          <>
            <label className="block text-sm font-medium text-slate-700 mb-1">
              {t("modal.modelLabel")}
            </label>
            <select
              className="w-full p-2 border border-slate-300 rounded mb-3 focus:outline-none focus:ring-2 focus:ring-blue-500 text-slate-900"
              value={model}
              onChange={(e) => setModel(e.target.value as AiConfig["model"])}
              disabled={isValidating}
            >
              {DEEPSEEK_MODELS.map((deepseekModel) => (
                <option key={deepseekModel} value={deepseekModel}>
                  {deepseekModel}
                </option>
              ))}
            </select>
          </>
        ) : (
          <p className="text-xs text-slate-500 mb-3">
            {t("modal.fixedModel", { model: effectiveModel })}
          </p>
        )}
        <input
          type="password"
          className="w-full p-2 border border-slate-300 rounded mb-4 focus:outline-none focus:ring-2 focus:ring-blue-500 text-slate-900"
          placeholder={t("modal.apiKeyPlaceholder")}
          value={apiKey}
          onChange={(e) => setApiKey(e.target.value)}
          disabled={isValidating}
        />
        {error && <p className="text-red-500 mb-4 text-sm">{error}</p>}
        <div className="flex justify-between items-center">
          <button
            onClick={handleDelete}
            className="text-red-500 hover:text-red-700 text-sm font-medium disabled:opacity-50"
            disabled={isValidating || !apiKey}
          >
            {t("modal.deleteKey")}
          </button>
          <div className="flex gap-2">
            <button
              onClick={onClose}
              className="px-4 py-2 text-slate-600 hover:bg-slate-100 rounded-md transition-colors disabled:opacity-50"
              disabled={isValidating}
            >
              {t("modal.cancel")}
            </button>
            <button
              onClick={handleSave}
              className="px-4 py-2 bg-blue-600 text-white rounded-md hover:bg-blue-700 transition-colors disabled:opacity-50"
              disabled={isValidating}
            >
              {isValidating ? t("modal.validating") : t("modal.save")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
