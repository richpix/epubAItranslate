import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import {
  Navigation24Regular,
  Key24Regular,
} from "@fluentui/react-icons";

// Componente para la barra lateral de navegación (Sidebar)
export function Sidebar({ onOpenApiKey }: { onOpenApiKey: () => void }) {
  const { t, i18n } = useTranslation();
  const [isCollapsed, setIsCollapsed] = useState(false);

// Función para alternar el idioma entre Español y el previamente soportado (Inglés por defecto como base de i18n)
  const toggleLanguage = () => {
    const nextLang = i18n.language.startsWith("es") ? "en" : "es";
    i18n.changeLanguage(nextLang);
  };

  return (
    <aside
      className={`h-screen bg-white text-slate-900 border-r border-slate-200 transition-all duration-300 flex flex-col ${
        isCollapsed ? "w-16" : "w-64"
      } max-md:w-16`}
    >
      {/* Header */}
      <div className="flex items-center justify-between p-4 h-16 border-b border-slate-100">
        <span
          className={`font-bold text-lg whitespace-nowrap overflow-hidden transition-opacity duration-300 ${
            isCollapsed ? "opacity-0 hidden max-md:hidden" : "opacity-100"
          } max-md:hidden`}
        >
          {t("sidebar.title")}
        </span>
        <button
          onClick={() => setIsCollapsed(!isCollapsed)}
          className="p-1 rounded-md hover:bg-slate-100 transition-colors mx-auto max-md:hidden"
          title={t("sidebar.toggle")}
        >
          <Navigation24Regular />
        </button>
      </div>

      <div className="flex-1" />

      {/* Footer / Settings */}
      <div className="p-2 border-t border-slate-100 flex flex-col gap-2 relative">
        <NavItem
          icon={
            <div className="flex items-center justify-center font-bold text-xs uppercase w-full h-full bg-slate-100 rounded-sm">
              {i18n.language.substring(0, 2)}
            </div>
          }
          label={t("sidebar.switchLanguage")}
          isCollapsed={isCollapsed}
          onClick={toggleLanguage}
        />
        <NavItem
          icon={<Key24Regular />}
          label={t("sidebar.apiKey") || "AI Key"}
          isCollapsed={isCollapsed}
          onClick={onOpenApiKey}
        />
      </div>
    </aside>
  );
}

// Componente secundario para definir y renderizar ítems listados en el Sidebar
function NavItem({
  icon,
  label,
  isCollapsed,
  onClick,
}: {
  icon: ReactNode;
  label: string;
  isCollapsed: boolean;
  onClick?: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="flex items-center gap-3 p-3 rounded-md hover:bg-slate-100 transition-colors w-full text-left"
      title={isCollapsed ? label : undefined}
    >
      <div className="shrink-0 flex items-center justify-center w-6 h-6">
        {icon}
      </div>
      <span
        className={`whitespace-nowrap overflow-hidden transition-all duration-300 ${
          isCollapsed ? "opacity-0 w-0 hidden max-md:hidden" : "opacity-100 w-auto"
        } max-md:hidden`}
      >
        {label}
      </span>
    </button>
  );
}
