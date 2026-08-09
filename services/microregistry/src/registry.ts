const CATALOG_KEY = "catalog.json";

export function registryObjectKey(pathname: string): string | null {
  if (!pathname.startsWith("/") || pathname === "/") return null;

  let key: string;
  try {
    key = decodeURIComponent(pathname.slice(1));
  } catch {
    return null;
  }

  if (!/^[A-Za-z0-9][A-Za-z0-9._/-]*$/.test(key)) return null;

  const segments = key.split("/");
  if (segments.some((segment) => segment === "" || segment.startsWith("."))) {
    return null;
  }

  return key;
}

export function isPublishedRegistryObject(key: string): boolean {
  if (key === CATALOG_KEY) return true;
  if (!key.includes("/")) return false;

  return key.endsWith(".tar.zst") || key.endsWith("/SHA256SUMS");
}

export function contentTypeFor(key: string): string {
  if (key === CATALOG_KEY) return "application/json; charset=utf-8";
  if (key.endsWith(".tar.zst")) return "application/zstd";
  return "text/plain; charset=utf-8";
}
