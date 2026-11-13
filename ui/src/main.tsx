import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { App } from "./App";
import { WorkloadLogsView } from "./components/WorkloadLogsView";
import { loadAppInfo } from "./ipc";
import { parseLogsRoute } from "./logsRoute";
import { useContextsStore } from "./stores/contexts";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("#root element missing");

const route = parseLogsRoute(window.location.hash);

if (route.kind === "logs") {
  createRoot(root).render(
    <StrictMode>
      <WorkloadLogsView detached target={route.target} />
    </StrictMode>,
  );
} else if (route.kind === "invalid") {
  createRoot(root).render(
    <StrictMode>
      <div className="workload-logs__route-error" role="alert">
        <h2>Cannot open logs</h2>
        <p>{route.message}</p>
      </div>
    </StrictMode>,
  );
} else {
  void loadAppInfo();
  void useContextsStore.getState().loadContexts();

  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
