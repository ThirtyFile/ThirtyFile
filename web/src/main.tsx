import React from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ApiError } from "@/api";
import { Toaster } from "@/components/ui/sonner";
import { ErrorBoundary } from "@/components/ErrorBoundary";
import "./style.css";
import { loadDictionary } from "@/lib/i18n";
import { reloadForNewVersion } from "@/lib/reload";
import { describe, installErrorReporting, report } from "@/lib/errorReport";

// Uncaught errors and rejected promises are reported to the error log (Control panel > Activity > Errors)
installErrorReporting();

// After a site update (redeploy), the code chunks an old page wants to load no longer exist: reload to get the new version
window.addEventListener("vite:preloadError", (e) => {
  if (reloadForNewVersion()) e.preventDefault();
});

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      refetchOnWindowFocus: false,
      staleTime: 5_000,
      // 4xx errors don't need a retry
      retry: (count, err) => !(err instanceof ApiError && err.status < 500) && count < 2,
    },
  },
});

// Load the translations first (a separate chunk, only for Traditional Chinese), then the app: its modules may call t() while being evaluated
loadDictionary()
  .then(() => import("@/App"))
  .then((mod) => {
    // A chunk that failed to load (deployment in progress, offline) can resolve to nothing instead of throwing
    if (!mod?.App) throw new Error("The app failed to load");
    const { App } = mod;
    createRoot(document.getElementById("root")!).render(
      <React.StrictMode>
        <QueryClientProvider client={queryClient}>
          <BrowserRouter>
            <ErrorBoundary>
              <App />
            </ErrorBoundary>
          </BrowserRouter>
          {/* --tf-bottom-inset: room for the phone selection bar while it shows */}
          <Toaster
            position="bottom-center"
            offset={{ bottom: "calc(var(--tf-bottom-inset, 0px) + 24px)" }}
            mobileOffset={{ bottom: "calc(var(--tf-bottom-inset, 0px) + 16px)" }}
          />
        </QueryClientProvider>
      </React.StrictMode>,
    );
  })
  .catch((e: unknown) => {
    // Chunks missing after a redeploy are handled by vite:preloadError above; anything else (offline, blocked script) ends here
    console.error(e);
    void report(describe("uncaught", e, "load"));
    const el = document.getElementById("root");
    if (el) el.textContent = "The app couldn't be loaded. Reload the page to try again.";
  });
