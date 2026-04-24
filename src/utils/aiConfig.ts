import { load } from "@tauri-apps/plugin-store";
import {
  DEEPSEEK_MODELS,
  DEFAULT_AI_CONFIG,
  GEMINI_MODEL,
  OPENAI_MODEL,
  type AiConfig,
  type AiModel,
  type AiProvider,
} from "../types/ai";

const CONFIG_FILE = ".config.dat";
const AI_CONFIG_KEY = "ai_config";
const LEGACY_DEEPSEEK_KEY = "deepseek_api_key";

function isProvider(value: unknown): value is AiProvider {
  return value === "deepseek" || value === "gemini" || value === "openai";
}

function normalizeModel(provider: AiProvider, value: unknown): AiModel {
  if (provider === "deepseek") {
    return DEEPSEEK_MODELS.includes(value as (typeof DEEPSEEK_MODELS)[number])
      ? (value as (typeof DEEPSEEK_MODELS)[number])
      : DEEPSEEK_MODELS[0];
  }
  if (provider === "gemini") {
    return GEMINI_MODEL;
  }
  return OPENAI_MODEL;
}

function normalizeConfig(raw: unknown): AiConfig | null {
  if (!raw || typeof raw !== "object") {
    return null;
  }
  const parsed = raw as Record<string, unknown>;
  if (!isProvider(parsed.provider)) {
    return null;
  }

  const apiKey = typeof parsed.apiKey === "string" ? parsed.apiKey : "";
  return {
    provider: parsed.provider,
    apiKey,
    model: normalizeModel(parsed.provider, parsed.model),
  };
}

export async function loadAiConfigFromStore(): Promise<AiConfig | null> {
  const store = await load(CONFIG_FILE);
  const rawConfig = await store.get<unknown>(AI_CONFIG_KEY);
  const config = normalizeConfig(rawConfig);
  if (config) {
    return config;
  }

  const legacyKey = await store.get<string | null | undefined>(LEGACY_DEEPSEEK_KEY);
  if (legacyKey && legacyKey.trim()) {
    const migrated: AiConfig = {
      ...DEFAULT_AI_CONFIG,
      apiKey: legacyKey,
    };
    await store.set(AI_CONFIG_KEY, migrated);
    await store.delete(LEGACY_DEEPSEEK_KEY);
    await store.save();
    return migrated;
  }

  return null;
}

export async function saveAiConfigToStore(config: AiConfig): Promise<void> {
  const store = await load(CONFIG_FILE);
  await store.set(AI_CONFIG_KEY, config);
  await store.delete(LEGACY_DEEPSEEK_KEY);
  await store.save();
}

export async function clearAiConfigInStore(): Promise<void> {
  const store = await load(CONFIG_FILE);
  await store.delete(AI_CONFIG_KEY);
  await store.delete(LEGACY_DEEPSEEK_KEY);
  await store.save();
}
