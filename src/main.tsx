import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./i18n/config";

// Punto de entrada principal para renderizar la aplicación de React dentro del contenedor root
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
