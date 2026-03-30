import i18n from "i18next";
import { initReactI18next } from "react-i18next";
import enTranslations from "./locales/en.json";
import esTranslations from "./locales/es.json";

i18n.use(initReactI18next).init({
  resources: {
    en: { translation: enTranslations },
    es: { translation: esTranslations },
  },
  lng: "es", // Establecemos español como idioma por defecto
  fallbackLng: "en",
  interpolation: {
    escapeValue: false, // React ya previene inyecciones XSS
  },
});

export default i18n;
