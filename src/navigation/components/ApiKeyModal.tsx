import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { load } from "@tauri-apps/plugin-store";
import { invoke } from "@tauri-apps/api/core";

const DEFAULT_AI_MODEL = "deepseek-chat";
const MODEL_OPTIONS = [
  { value: "deepseek-chat", label: "DeepSeek Chat" },
  { value: "gemini-2.5-flash-lite", label: "Gemini 2.5 Flash Lite" },
];

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
  const [apiKey, setApiKey] = useState("");
  const [model, setModel] = useState(DEFAULT_AI_MODEL);
  const [isValidating, setIsValidating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (isOpen) {
      loadStore();
    }
  }, [isOpen]);

  const loadStore = async () => {
    try {
      const store = await load(".config.dat");
      const legacyDeepseekKey: string | null | undefined = await store.get("deepseek_api_key");
      const savedKey: string | null | undefined =
        (await store.get("ai_api_key")) ?? legacyDeepseekKey;
      const savedModel: string | null | undefined = await store.get("ai_model");
      if (savedKey) {
        setApiKey(savedKey);
      }
      setModel(savedModel || (legacyDeepseekKey ? "deepseek-chat" : DEFAULT_AI_MODEL));
    } catch (err) {
      console.error("Failed to load store", err);
    }
  };

  const handleSave = async () => {
    if (!apiKey.trim()) {
      setError(t("modal.errorEmpty"));
      return;
    }
    setIsValidating(true);
    setError(null);

    try {
      const isValid = await invoke("validate_api_key", {
        request: {
          apiKey,
          model,
        },
      });
      if (isValid) {
        const store = await load(".config.dat");
        await store.set("ai_api_key", apiKey);
        await store.set("ai_model", model);
        await store.delete("deepseek_api_key");
        await store.save();
        onApiKeyChanged?.();
        onClose();
      } else {
        setError(t("modal.errorInvalid"));
      }
    } catch (err: any) {
      setError(err.toString());
    } finally {
      setIsValidating(false);
    }
  };

  const handleDelete = async () => {
    try {
      const store = await load(".config.dat");
      await store.delete("ai_api_key");
      await store.delete("ai_model");
      await store.delete("deepseek_api_key");
      await store.save();
      setApiKey("");
      setModel(DEFAULT_AI_MODEL);
      onApiKeyChanged?.();
      onClose();
    } catch (err) {
      console.error("Failed to delete key", err);
    }
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
      <div className="bg-white p-6 rounded-lg shadow-xl w-96 max-w-[90vw]">
        <h2 className="text-xl font-bold mb-4 text-slate-900">
          {t("modal.apiKeyTitle")}
        </h2>
        <label className="text-sm font-medium text-slate-700 mb-2 block" htmlFor="ai-model-select">
          {t("modal.modelLabel")}
        </label>
        <select
          id="ai-model-select"
          className="w-full p-2 border border-slate-300 rounded mb-3 focus:outline-none focus:ring-2 focus:ring-blue-500 text-slate-900 bg-white"
          value={model}
          onChange={(e) => setModel(e.target.value)}
          disabled={isValidating}
        >
          {MODEL_OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
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
