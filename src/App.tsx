import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { Sidebar } from "./navigation/components/Sidebar";
import { ApiKeyModal } from "./navigation/components/ApiKeyModal";
import { TranslationPanel } from "./components/TranslationPanel";
import type { AiConfig } from "./types/ai";
import { loadAiConfigFromStore } from "./utils/aiConfig";
import "./App.css";

// Componente raíz de la aplicación web
function App() {
  const { t } = useTranslation();
  const [isApiKeyModalOpen, setIsApiKeyModalOpen] = useState(false);
  const [aiConfig, setAiConfig] = useState<AiConfig | null>(null);

// Función de carga de API key almacenada en Tauri Store
  const loadAiConfig = async () => {
    try {
      const loadedConfig = await loadAiConfigFromStore();
      setAiConfig(loadedConfig);
    } catch (error) {
      console.error("Failed to load API key", error);
      setAiConfig(null);
    }
  };

  useEffect(() => {
    void loadAiConfig();
  }, []);

  return (
    <div className="flex h-screen bg-white text-slate-900 overflow-hidden">
      <Sidebar onOpenApiKey={() => setIsApiKeyModalOpen(true)} />
      <main className="flex-1 overflow-auto bg-slate-50">
        <header className="p-8 border-b border-slate-200 bg-white">
          <h1 className="text-3xl font-bold">{t("home.title")}</h1>
          <p className="text-slate-500 mt-2">{t("home.description")}</p>
        </header>
        <div className="p-8">
          <TranslationPanel
            aiConfig={aiConfig}
            hasApiKey={Boolean(aiConfig?.apiKey)}
            onOpenApiKey={() => setIsApiKeyModalOpen(true)}
          />

          <p className="text-slate-600 text-sm mt-4">{t("home.content")}</p>
        </div>
      </main>

      <ApiKeyModal
        isOpen={isApiKeyModalOpen}
        onClose={() => setIsApiKeyModalOpen(false)}
        onApiKeyChanged={() => {
// Recarga la vista si la API Key cambia
          void loadAiConfig();
        }}
      />
    </div>
  );
}

export default App;
