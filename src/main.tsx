import React from "react";
import ReactDOM from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
import "./theme.css";
import "./styles.css";
import "./styles-acp.css";
import "./styles-spec.css";
import "./styles-runs.css";
import App from "./App";
import { paintRemembered } from "./lib/theme";

paintRemembered();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
