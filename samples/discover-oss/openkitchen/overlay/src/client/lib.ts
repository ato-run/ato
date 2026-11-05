import { useCallback, useEffect, useState } from "react";
import { localApi } from "./local-api";

export async function api<T = unknown>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  let res: Response;
  try {
    res = await localApi(path, {
      ...init,
      headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
    });
  } catch (error) {
    window.dispatchEvent(new CustomEvent("openkitchen-storage-error", { detail: String(error) }));
    throw error;
  }
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`);
  return res.json() as Promise<T>;
}

export function useApi<T>(path: string) {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const reload = useCallback(() => {
    setLoading(true);
    api<T>(path)
      .then((d) => {
        setData(d);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [path]);
  useEffect(() => reload(), [reload]);
  return { data, loading, error, reload };
}

export function money(cents: number | null | undefined, currency = "EUR"): string {
  if (cents == null) return "—";
  return new Intl.NumberFormat("en-IE", {
    style: "currency",
    currency,
  }).format(cents / 100);
}

export function pct(n: number | null | undefined): string {
  return n == null ? "—" : `${n.toFixed(1)}%`;
}

export function qty(n: number | null | undefined, unit?: string | null): string {
  if (n == null) return "—";
  const s = Number.isInteger(n) ? String(n) : n.toFixed(2);
  return unit ? `${s} ${unit}` : s;
}
