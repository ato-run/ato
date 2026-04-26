"use client";

import { useCallback, useEffect, useState, useRef } from "react";

/**
 * Manifest metadata for .sync file
 *
 * The Capsule app controls all metadata - the runtime is domain-agnostic.
 */
export interface SyncManifestMeta {
  /** Content type (e.g., "application/vnd.ato.chat+json") */
  contentType: string;
  /** Display extension (e.g., "chat", "canvas", "note") */
  displayExt?: string;
  /** Human-readable title */
  title?: string;
  /** Variant (e.g., "data", "vault") */
  variant?: string;
  /** Additional app-specific metadata */
  extra?: Record<string, unknown>;
}

/**
 * Request to write a .sync file
 */
export interface WriteSyncRequest {
  /** Relative path within ~/Ato/Data/ (e.g., "Chats/2026-02-05-MyChat.chat.sync") */
  path: string;
  /** Manifest metadata */
  manifest: SyncManifestMeta;
  /** Payload data (any JSON-serializable structure) */
  payload: unknown;
  /** Whether to encrypt the payload */
  encrypt?: boolean;
}

/**
 * Save state for UI feedback
 */
export type SaveState = "idle" | "saving" | "saved" | "error";

/**
 * Response from Host when write completes
 */
interface WriteCompletePayload {
  success: true;
  path: string;
  filename: string;
  size: number;
}

/**
 * Response from Host when write fails
 */
interface WriteErrorPayload {
  success: false;
  error: string;
  code?: string;
}

/**
 * Message structure from Host
 */
interface HostMessage {
  type: string;
  payload:
    | WriteCompletePayload
    | WriteErrorPayload
    | { success: boolean; files?: string[] }
    | {
        success: boolean;
        manifest?: unknown;
        payload?: unknown;
        error?: string;
      };
  requestId?: string;
}

/**
 * Check if running inside Ato Dashboard (iframe)
 */
function isInAtoDashboard(): boolean {
  if (typeof window === "undefined") return false;
  try {
    return window.parent !== window;
  } catch {
    return false;
  }
}

/**
 * Generate a unique request ID
 */
function generateRequestId(): string {
  return `req_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
}

/**
 * Hook for generic Sync I/O via Ato Host Bridge
 *
 * This hook is domain-agnostic. The Capsule app controls:
 * - File paths and naming
 * - Manifest metadata (content_type, title, etc.)
 * - Payload structure
 *
 * @example
 * ```tsx
 * const { saveSync, saveState, isAtoEnvironment } = useAtoBridge();
 *
 * // App controls all metadata
 * await saveSync({
 *   path: `Chats/${date}-${title}.chat.sync`,
 *   manifest: {
 *     contentType: 'application/vnd.ato.chat+json',
 *     title: 'My Chat',
 *     extra: { model: 'gpt-4o' }
 *   },
 *   payload: { messages: [...] }
 * });
 * ```
 */
export function useAtoBridge() {
  const [saveState, setSaveState] = useState<SaveState>("idle");
  const [lastSavedAt, setLastSavedAt] = useState<Date | null>(null);
  const [lastSavedPath, setLastSavedPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const pendingRequests = useRef<
    Map<
      string,
      (result: { success: boolean; path?: string; error?: string }) => void
    >
  >(new Map());
  const saveTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  const isAtoEnvironment = isInAtoDashboard();

  /**
   * Send message to Host (Dashboard)
   */
  const sendToHost = useCallback(
    (type: string, payload: unknown, requestId?: string) => {
      if (!isAtoEnvironment) {
        console.warn(
          "[useAtoBridge] Not in Ato environment, cannot send message",
        );
        return;
      }

      window.parent.postMessage({ type, payload, requestId }, "*");
      console.log("[useAtoBridge] Sent to host:", type);
    },
    [isAtoEnvironment],
  );

  /**
   * Handle messages from Host
   */
  useEffect(() => {
    if (!isAtoEnvironment) return;

    const handleMessage = (event: MessageEvent) => {
      const data = event.data as HostMessage;
      if (!data || typeof data.type !== "string") return;

      // Only handle ATO_FS_* responses
      if (!data.type.startsWith("ATO_FS_")) return;

      console.log("[useAtoBridge] Received from host:", data.type);

      switch (data.type) {
        case "ATO_FS_WRITE_COMPLETE": {
          const payload = data.payload as WriteCompletePayload;
          setSaveState("saved");
          setLastSavedAt(new Date());
          setLastSavedPath(payload.path);
          setError(null);

          // Resolve pending request
          if (data.requestId && pendingRequests.current.has(data.requestId)) {
            pendingRequests.current.get(data.requestId)?.({
              success: true,
              path: payload.path,
            });
            pendingRequests.current.delete(data.requestId);
          }

          // Reset to idle after 2 seconds
          if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
          saveTimeoutRef.current = setTimeout(() => setSaveState("idle"), 2000);
          break;
        }

        case "ATO_FS_WRITE_ERROR": {
          const payload = data.payload as WriteErrorPayload;
          setSaveState("error");
          setError(payload.error);

          // Resolve pending request
          if (data.requestId && pendingRequests.current.has(data.requestId)) {
            pendingRequests.current.get(data.requestId)?.({
              success: false,
              error: payload.error,
            });
            pendingRequests.current.delete(data.requestId);
          }

          // Reset to idle after 3 seconds
          if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
          saveTimeoutRef.current = setTimeout(() => {
            setSaveState("idle");
            setError(null);
          }, 3000);
          break;
        }

        case "ATO_FS_LIST_COMPLETE":
        case "ATO_FS_LIST_ERROR": {
          // Resolve pending list request
          if (data.requestId && pendingRequests.current.has(data.requestId)) {
            const payload = data.payload as {
              success: boolean;
              files?: string[];
              error?: string;
            };
            pendingRequests.current.get(data.requestId)?.(payload);
            pendingRequests.current.delete(data.requestId);
          }
          break;
        }
        case "ATO_FS_READ_COMPLETE":
        case "ATO_FS_READ_ERROR": {
          if (data.requestId && pendingRequests.current.has(data.requestId)) {
            const payload = data.payload as {
              success: boolean;
              manifest?: unknown;
              payload?: unknown;
              error?: string;
            };
            pendingRequests.current.get(data.requestId)?.(payload);
            pendingRequests.current.delete(data.requestId);
          }
          break;
        }
      }
    };

    window.addEventListener("message", handleMessage);
    console.log("[useAtoBridge] Message listener registered");

    return () => {
      window.removeEventListener("message", handleMessage);
      if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
    };
  }, [isAtoEnvironment]);

  /**
   * Save data to a .sync file
   *
   * The Capsule app controls:
   * - File path (relative to ~/Ato/Data/)
   * - Manifest metadata
   * - Payload structure
   *
   * @param request - Write request with path, manifest, and payload
   * @returns Promise that resolves when save completes
   */
  const saveSync = useCallback(
    async (
      request: WriteSyncRequest,
    ): Promise<{ success: boolean; path?: string; error?: string }> => {
      if (!isAtoEnvironment) {
        console.warn(
          "[useAtoBridge] Not in Ato environment, falling back to localStorage",
        );
        // Fallback to localStorage for standalone mode
        try {
          localStorage.setItem(`sync_${request.path}`, JSON.stringify(request));
          return { success: true, path: request.path };
        } catch (e) {
          return { success: false, error: String(e) };
        }
      }

      setSaveState("saving");
      setError(null);

      const requestId = generateRequestId();

      return new Promise((resolve) => {
        // Set up timeout for request
        const timeout = setTimeout(() => {
          if (pendingRequests.current.has(requestId)) {
            pendingRequests.current.delete(requestId);
            setSaveState("error");
            setError("Save request timed out");
            resolve({ success: false, error: "Save request timed out" });
          }
        }, 10000); // 10 second timeout

        pendingRequests.current.set(
          requestId,
          (result: { success: boolean; path?: string; error?: string }) => {
            clearTimeout(timeout);
            resolve(result);
          },
        );

        sendToHost(
          "ATO_FS_WRITE_SYNC",
          {
            path: request.path,
            manifest: request.manifest,
            payload: request.payload,
            encrypt: request.encrypt,
          },
          requestId,
        );
      });
    },
    [isAtoEnvironment, sendToHost],
  );

  /**
   * List .sync files in a directory
   *
   * @param directory - Relative directory path (e.g., "Chats")
   * @returns Promise with list of file paths
   */
  const listSync = useCallback(
    async (
      directory: string,
    ): Promise<{ success: boolean; files?: string[]; error?: string }> => {
      if (!isAtoEnvironment) {
        console.warn("[useAtoBridge] Not in Ato environment");
        return { success: false, error: "Not in Ato environment" };
      }

      const requestId = generateRequestId();

      return new Promise((resolve) => {
        const timeout = setTimeout(() => {
          if (pendingRequests.current.has(requestId)) {
            pendingRequests.current.delete(requestId);
            resolve({ success: false, error: "List request timed out" });
          }
        }, 10000);

        pendingRequests.current.set(
          requestId,
          (result: { success: boolean; files?: string[]; error?: string }) => {
            clearTimeout(timeout);
            resolve(
              result as { success: boolean; files?: string[]; error?: string },
            );
          },
        );

        sendToHost("ATO_FS_LIST_SYNC", { directory }, requestId);
      });
    },
    [isAtoEnvironment, sendToHost],
  );

  /**
   * Read a .sync file
   *
   * @param path - Relative path within ~/Ato/Data/ (e.g., "Chats/2026-02-05-MyChat.chat.sync")
   * @returns Promise with manifest + payload
   */
  const readSync = useCallback(
    async (
      path: string,
    ): Promise<{
      success: boolean;
      manifest?: unknown;
      payload?: unknown;
      error?: string;
    }> => {
      if (!isAtoEnvironment) {
        console.warn("[useAtoBridge] Not in Ato environment");
        return { success: false, error: "Not in Ato environment" };
      }

      const requestId = generateRequestId();

      return new Promise((resolve) => {
        const timeout = setTimeout(() => {
          if (pendingRequests.current.has(requestId)) {
            pendingRequests.current.delete(requestId);
            resolve({ success: false, error: "Read request timed out" });
          }
        }, 10000);

        pendingRequests.current.set(
          requestId,
          (result: {
            success: boolean;
            manifest?: unknown;
            payload?: unknown;
            error?: string;
          }) => {
            clearTimeout(timeout);
            resolve(
              result as {
                success: boolean;
                manifest?: unknown;
                payload?: unknown;
                error?: string;
              },
            );
          },
        );

        sendToHost("ATO_FS_READ_SYNC", { path }, requestId);
      });
    },
    [isAtoEnvironment, sendToHost],
  );

  /**
   * Create a debounced auto-save function
   *
   * @param delayMs - Delay in milliseconds (default: 2000)
   * @returns Debounced save function
   */
  const createAutoSave = useCallback(
    (delayMs = 2000) => {
      let timeoutId: ReturnType<typeof setTimeout> | null = null;

      return (request: WriteSyncRequest) => {
        if (timeoutId) clearTimeout(timeoutId);
        timeoutId = setTimeout(() => {
          saveSync(request);
        }, delayMs);
      };
    },
    [saveSync],
  );

  return {
    /** Save data to a .sync file (generic) */
    saveSync,
    /** List .sync files in a directory */
    listSync,
    /** Read a .sync file */
    readSync,
    /** Create a debounced auto-save function */
    createAutoSave,
    /** Current save state ('idle' | 'saving' | 'saved' | 'error') */
    saveState,
    /** Last successful save timestamp */
    lastSavedAt,
    /** Last saved file path */
    lastSavedPath,
    /** Error message if save failed */
    error,
    /** Whether running inside Ato Dashboard */
    isAtoEnvironment,
  };
}

export default useAtoBridge;
