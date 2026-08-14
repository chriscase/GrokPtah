import React from "react";
import ReactDOM from "react-dom/client";
import { ErrorBoundary } from "./components/ErrorBoundary";
import "./styles/app.css";

const root = ReactDOM.createRoot(document.getElementById("root")!);
const computerStory =
  import.meta.env.DEV &&
  new URLSearchParams(window.location.search).get("story") === "computer";

void (computerStory
  ? import("./components/ComputerCockpitStory").then(({ ComputerCockpitStory }) =>
      root.render(
        <React.StrictMode>
          <ErrorBoundary label="computer cockpit story">
            <ComputerCockpitStory />
          </ErrorBoundary>
        </React.StrictMode>,
      ),
    )
  : import("./App").then(({ default: App }) =>
      root.render(
        <React.StrictMode>
          <ErrorBoundary label="app">
            <App />
          </ErrorBoundary>
        </React.StrictMode>,
      ),
    ));
