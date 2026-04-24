export type AiProvider = "deepseek" | "gemini" | "openai";

export const DEEPSEEK_MODELS = ["deepseek-v4-pro", "deepseek-v4-flash"] as const;
export const GEMINI_MODEL = "gemini-3.1-flash-lite-preview";
export const OPENAI_MODEL = "gpt-5.4-nano";

export type DeepSeekModel = (typeof DEEPSEEK_MODELS)[number];
export type AiModel = DeepSeekModel | typeof GEMINI_MODEL | typeof OPENAI_MODEL;

export type AiConfig = {
  provider: AiProvider;
  apiKey: string;
  model: AiModel;
};

export const DEFAULT_AI_CONFIG: AiConfig = {
  provider: "deepseek",
  apiKey: "",
  model: DEEPSEEK_MODELS[0],
};
