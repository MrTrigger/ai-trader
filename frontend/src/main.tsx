import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import "./index.css";

const qc = new QueryClient({
  defaultOptions: { queries: { retry: 1, refetchOnWindowFocus: true } },
});

// The legacy page routed on the hash (#/bot/x); this one uses real paths.
// Bookmarks and open tabs still carry the old form, so translate rather
// than silently render the fleet and look broken.
if (location.hash.startsWith("#/")) {
  const path = location.hash.slice(1);
  history.replaceState(null, "", path === "/" ? "/" : path);
}

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={qc}>
      <BrowserRouter>
        <App />
      </BrowserRouter>
    </QueryClientProvider>
  </StrictMode>,
);
