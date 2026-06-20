const KEY_REGISTRY_AUTH_TOKEN = "ato.local.registry.auth-token.v1";

export function loadRegistryAuthToken(): string {
  return localStorage.getItem(KEY_REGISTRY_AUTH_TOKEN)?.trim() ?? "";
}

export function saveRegistryAuthToken(token: string): void {
  const trimmed = token.trim();
  if (!trimmed) {
    localStorage.removeItem(KEY_REGISTRY_AUTH_TOKEN);
    return;
  }
  localStorage.setItem(KEY_REGISTRY_AUTH_TOKEN, trimmed);
}
