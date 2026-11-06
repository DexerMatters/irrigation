import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./app";
import "./design/canvas.css";
import "./design/tokens.css";
import "./design/app.css";

const root = document.getElementById("root");
if (!root) throw new Error("missing #root");
createRoot(root).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
