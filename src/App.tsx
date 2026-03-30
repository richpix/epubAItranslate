import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { load } from "@tauri-apps/plugin-store";
import { Sidebar } from "./navigation/components/Sidebar";
import { ApiKeyModal } from "./navigation/components/ApiKeyModal";
import { TranslationPanel } from "./components/TranslationPanel";
import "./App.css";

const DEFAULT_AI_MODEL = "deepseek-chat";

function App() {
  const { t } = useTranslation();
  const [isApiKeyModalOpen, setIsApiKeyModalOpen] = useState(false);
  const [apiKey, setApiKey] = useState<string | null>(null);
  const [aiModel, setAiModel] = useState(DEFAULT_AI_MODEL);

  useEffect(() => {
    void loadApiKey();
  }, []);

  const loadApiKey = async () => {
    try {
      const store = await load(".config.dat");
      const legacyDeepseekKey: string | null | undefined = await store.get("deepseek_api_key");
      const savedKey: string | null | undefined =
        (await store.get("ai_api_key")) ?? legacyDeepseekKey;
      const savedModel: string | null | undefined = await store.get("ai_model");
      setApiKey(savedKey ?? null);
      setAiModel(savedModel ?? (legacyDeepseekKey ? "deepseek-chat" : DEFAULT_AI_MODEL));
    } catch (error) {
      console.error("Failed to load API key", error);
      setApiKey(null);
      setAiModel(DEFAULT_AI_MODEL);
    }
  };

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
            apiKey={apiKey}
            aiModel={aiModel}
            hasApiKey={Boolean(apiKey)}
            onOpenApiKey={() => setIsApiKeyModalOpen(true)}
          />

          <p className="text-slate-600 text-sm mt-4">{t("home.content")}</p>
        </div>
      </main>

      <ApiKeyModal
        isOpen={isApiKeyModalOpen}
        onClose={() => setIsApiKeyModalOpen(false)}
        onApiKeyChanged={() => {
          void loadApiKey();
        }}
      />
    </div>
  );
}

export default App;
