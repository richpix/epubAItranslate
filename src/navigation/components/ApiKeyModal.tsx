import { useState, useEffect } from "react";
import { useTranslation } from "react-i18next";
import { load } from "@tauri-apps/plugin-store";
import { invoke } from "@tauri-apps/api/core";

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
  const [apiKey, setApiKey] = useState("");
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
      const store = await load(".config.dat");
      const savedKey: string | null | undefined = await store.get("deepseek_api_key");
      if (savedKey) {
        setApiKey(savedKey);
      }
    } catch (err) {
      console.error("Failed to load store", err);
    }
  };

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
      const isValid = await invoke("validate_api_key", { apiKey });
      if (isValid) {
// Almacena y persiste la nueva clave
        const store = await load(".config.dat");
        await store.set("deepseek_api_key", apiKey);
        await store.save();
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
      const store = await load(".config.dat");
      await store.delete("deepseek_api_key");
      await store.save();
      setApiKey("");
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
