import React, { lazy, Suspense } from "react";
import ReactDOM from "react-dom/client";
import { getCurrentWindow } from "@tauri-apps/api/window";
import App from "./App";
import { inTauri } from "./api";
import "./theme.css";
import "./settings/accents.css";
import "./styles.css";

// The Settings window (label "settings"; ?window=settings in a plain browser) shows only the settings.
const SettingsApp = lazy(() => import("./settings/SettingsApp"));
const isSettings = inTauri ? getCurrentWindow().label === "settings" : new URLSearchParams(location.search).get("window") === "settings";
if (isSettings) document.documentElement.classList.add("settings-window");

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {isSettings ? (
      <Suspense fallback={null}>
        <SettingsApp />
      </Suspense>
    ) : (
      <App />
    )}
  </React.StrictMode>,
);
