function readPublicEnv(key: string, fallback: string): string {
  const value = import.meta.env[key]?.trim();
  return value ? value : fallback;
}

export const publicEnv = {
  apiBaseUrl: readPublicEnv("VITE_API_BASE_URL", "/api/v1"),
  appName: readPublicEnv("VITE_APP_NAME", "crawler-media console"),
} as const;
