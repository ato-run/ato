import { useState, useEffect, useCallback } from "react";

/**
 * IPC bridge hook for all ato-launch / ato-dock wizard windows.
 * Handles both window.ipc (WKWebView) and window.__ATO_IPC__ (fallback).
 */
export function useBridge(capsule = "launch") {
  const send = useCallback(
    (command) => {
      const payload = JSON.stringify({ capsule, command });
      if (window.__ATO_IPC__?.postMessage) {
        window.__ATO_IPC__.postMessage(payload);
      } else if (window.ipc?.postMessage) {
        window.ipc.postMessage(payload);
      } else {
        console.debug("[ato-ui bridge missing]", command);
      }
    },
    [capsule]
  );

  return { send };
}

/**
 * Hook to subscribe to window.__ato_hydrate_preview(json) callbacks.
 * Calls `onPreview(data)` when Rust pushes a LaunchConsentPreview.
 */
export function usePreviewHydration(onPreview) {
  useEffect(() => {
    window.__ato_hydrate_preview = onPreview;
    // Check if preview was already injected before React mounted
    const initial = window.__ATO_LAUNCH_PREVIEW;
    if (initial && !initial.loading) onPreview(initial);
    return () => {
      delete window.__ato_hydrate_preview;
    };
  }, [onPreview]);
}
