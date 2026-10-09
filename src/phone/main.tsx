import React from "react";
import ReactDOM from "react-dom/client";
import "@xterm/xterm/css/xterm.css";
import "../theme.css";
import "./phone.css";
import { App } from "./App";
import { paintRemembered } from "../lib/theme";

paintRemembered();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
