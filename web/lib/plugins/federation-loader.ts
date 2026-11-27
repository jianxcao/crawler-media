import * as PluginUiKit from "@crawler-media/plugin-ui-kit";
import * as PluginUiSdk from "@crawler-media/plugin-ui-sdk";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import React, { type ComponentType } from "react";
import * as ReactDOM from "react-dom";
import * as ReactJsxRuntime from "react/jsx-runtime";

import type { UiDescriptorView } from "@/lib/api/plugins";

export interface ShareEntry {
  get: () => Promise<() => unknown>;
  loaded: 1;
  from: string;
  scope: string;
}

export type HostShareScope = Record<string, Record<string, ShareEntry>>;

export interface FederatedContainer {
  init: (shareScope: HostShareScope) => Promise<void> | void;
  get: (expose: string) => Promise<() => { default: ComponentType<PluginSurfaceProps> }>;
}

interface AssetManifestFile {
  path: string;
  content_type?: string;
}

export interface LoadSurfaceOptions {
  importRemote?: (url: string) => Promise<FederatedContainer>;
  fetchManifest?: (manifestUrl: string) => Promise<AssetManifestFile[]>;
  reloadPage?: () => void;
}

const containerCache = new Map<string, Promise<FederatedContainer>>();
const mountedStyles = new Map<string, HTMLLinkElement[]>();
const loadedRemoteHashes = new Map<string, string>();
let customReloadHandler: ((targetPath?: string) => void) | null = null;

export function setDocumentReloadHandlerForTest(
  handler: ((targetPath?: string) => void) | null,
): void {
  customReloadHandler = handler;
}

export function reloadCurrentDocument(targetPath?: string): void {
  if (customReloadHandler) {
    customReloadHandler(targetPath);
    return;
  }
  if (typeof window !== "undefined" && window.location) {
    if (targetPath && typeof window.location.replace === "function") {
      window.location.replace(targetPath);
      return;
    }
    if (targetPath && typeof window.location.assign === "function") {
      window.location.assign(targetPath);
      return;
    }
    if (typeof window.location.reload === "function") {
      window.location.reload();
    }
  }
}

export function isRemoteLoaded(remoteName: string): boolean {
  return loadedRemoteHashes.has(remoteName);
}

export function getLoadedRemoteHash(remoteName: string): string | undefined {
  return loadedRemoteHashes.get(remoteName);
}

function buildHostShareScope(): HostShareScope {
  return {
    react: {
      "19.3.0": {
        get: async () => () => React,
        loaded: 1,
        from: "crawler_media_host",
        scope: "default",
      },
    },
    "react-dom": {
      "19.3.0": {
        get: async () => () => ReactDOM,
        loaded: 1,
        from: "crawler_media_host",
        scope: "default",
      },
    },
    "react/jsx-runtime": {
      "19.3.0": {
        get: async () => () => ReactJsxRuntime,
        loaded: 1,
        from: "crawler_media_host",
        scope: "default",
      },
    },
    "@crawler-media/plugin-ui-sdk": {
      "1.0.0": {
        get: async () => () => PluginUiSdk,
        loaded: 1,
        from: "crawler_media_host",
        scope: "default",
      },
    },
    "@crawler-media/plugin-ui-kit": {
      "1.0.0": {
        get: async () => () => PluginUiKit,
        loaded: 1,
        from: "crawler_media_host",
        scope: "default",
      },
    },
  };
}

function defaultImportRemote(url: string): Promise<FederatedContainer> {
  return import(/* @vite-ignore */ url) as Promise<FederatedContainer>;
}

function isSafeRelativeAsset(rel: string): boolean {
  if (!rel || rel.startsWith("/") || rel.includes("..")) return false;
  return rel.endsWith(".css") && !rel.endsWith(".map") && !rel.endsWith(".html");
}

function ensurePluginStylesheets(
  pluginId: string,
  cacheKey: string,
  assetBaseUrl: string,
  files: AssetManifestFile[],
): void {
  if (typeof document === "undefined" || !document.head) return;
  if (mountedStyles.has(cacheKey)) return;

  for (const [key, links] of mountedStyles.entries()) {
    if (key.startsWith(`${pluginId}:`) && key !== cacheKey) {
      for (const link of links) link.remove();
      mountedStyles.delete(key);
    }
  }

  const base = assetBaseUrl.replace(/\/$/, "");
  const created: HTMLLinkElement[] = [];
  for (const file of files) {
    if (!isSafeRelativeAsset(file.path)) continue;
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = `${base}/${file.path}`;
    link.dataset.cmPluginUi = cacheKey;
    document.head.appendChild(link);
    created.push(link);
  }
  mountedStyles.set(cacheKey, created);
}

function removeStylesheetsForKey(cacheKey: string): void {
  const tracked = mountedStyles.get(cacheKey);
  if (tracked) {
    for (const link of tracked) link.remove();
    mountedStyles.delete(cacheKey);
  }
  if (typeof document !== "undefined" && document.head) {
    const links = Array.from(document.head.querySelectorAll("link[data-cm-plugin-ui]"));
    for (const link of links) {
      if (link.getAttribute("data-cm-plugin-ui") === cacheKey) {
        link.remove();
      }
    }
  }
}

export function descriptorCacheKey(
  descriptor: Pick<UiDescriptorView, "plugin_id" | "generation" | "bundle_sha256"> & {
    subject_user_id?: string;
  },
): string {
  return `${descriptor.plugin_id}:${descriptor.generation}:${descriptor.bundle_sha256}:${descriptor.subject_user_id ?? ""}`;
}

export async function loadFederatedSurface(
  descriptor: UiDescriptorView,
  expose: string,
  options: LoadSurfaceOptions = {},
): Promise<ComponentType<PluginSurfaceProps>> {
  const key = descriptorCacheKey(descriptor);
  const importRemote = options.importRemote ?? defaultImportRemote;
  const nextRemoteHash = `${descriptor.package_hash}:${descriptor.bundle_sha256}`;
  const prevRemoteHash = loadedRemoteHashes.get(descriptor.remote_name);

  if (prevRemoteHash !== undefined && prevRemoteHash !== nextRemoteHash) {
    evictPluginFederationCache(descriptor.plugin_id);
    if (options.reloadPage) {
      options.reloadPage();
    } else if (customReloadHandler) {
      customReloadHandler();
    } else if (!options.importRemote) {
      reloadCurrentDocument();
    }
  }

  let containerPromise = containerCache.get(key);
  if (!containerPromise) {
    const base = descriptor.asset_base_url.replace(/\/$/, "");
    let trackedPromise: Promise<FederatedContainer> | null = null;
    const nextPromise = (async () => {
      const assertStillActive = () => {
        if (trackedPromise !== null && containerCache.get(key) !== trackedPromise) {
          throw new Error(
            `Federated remote "${descriptor.remote_name}" for "${descriptor.plugin_id}" was evicted while loading`,
          );
        }
      };
      const files = options.fetchManifest
        ? await options.fetchManifest(`${base}/ui/asset-manifest.json`).catch(() => [])
        : Array.isArray(descriptor.styles)
          ? descriptor.styles.map((p) => ({ path: p }))
          : [];
      assertStillActive();
      const container = await importRemote(descriptor.entry_url);
      assertStillActive();
      if (!container || typeof container.init !== "function" || typeof container.get !== "function") {
        throw new Error(
          `Federated remote "${descriptor.remote_name}" for "${descriptor.plugin_id}" is not a valid container`,
        );
      }
      await container.init(buildHostShareScope());
      assertStillActive();
      ensurePluginStylesheets(descriptor.plugin_id, key, base, files);
      loadedRemoteHashes.set(descriptor.remote_name, nextRemoteHash);
      return container;
    })().catch((err) => {
      if (containerCache.get(key) === trackedPromise) {
        containerCache.delete(key);
        removeStylesheetsForKey(key);
      }
      throw err;
    });
    trackedPromise = nextPromise;
    containerCache.set(key, nextPromise);
    containerPromise = nextPromise;
  }

  const container = await containerPromise;
  const factory = await container.get(expose);
  if (containerCache.get(key) !== containerPromise) {
    throw new Error(
      `Federated remote "${descriptor.remote_name}" for "${descriptor.plugin_id}" was evicted while loading`,
    );
  }
  if (typeof factory !== "function") {
    throw new Error(
      `Federated surface "${expose}" in "${descriptor.plugin_id}" did not return a module factory function`,
    );
  }
  const mod = factory() as { default?: unknown } | null | undefined;
  const candidate = mod?.default;
  const isValidComponent =
    typeof candidate === "function" || (typeof candidate === "object" && candidate !== null);
  if (!isValidComponent) {
    throw new Error(
      `Federated surface "${expose}" in "${descriptor.plugin_id}" does not export a valid default React component`,
    );
  }
  return candidate as ComponentType<PluginSurfaceProps>;
}

export function evictPluginFederationCache(pluginId: string): void {
  const prefix = `${pluginId}:`;
  for (const key of Array.from(containerCache.keys())) {
    if (key.startsWith(prefix)) {
      containerCache.delete(key);
    }
  }
  for (const [key, links] of Array.from(mountedStyles.entries())) {
    if (key.startsWith(prefix)) {
      for (const link of links) link.remove();
      mountedStyles.delete(key);
    }
  }
  if (typeof document !== "undefined" && document.head) {
    const links = Array.from(document.head.querySelectorAll("link[data-cm-plugin-ui]"));
    for (const link of links) {
      const attr = link.getAttribute("data-cm-plugin-ui") ?? "";
      if (attr.startsWith(prefix)) {
        link.remove();
      }
    }
  }
}

export function clearFederationCache(): void {
  containerCache.clear();
  loadedRemoteHashes.clear();
  for (const links of mountedStyles.values()) {
    for (const link of links) link.remove();
  }
  mountedStyles.clear();
  if (typeof document !== "undefined" && document.head) {
    for (const link of Array.from(document.head.querySelectorAll("link[data-cm-plugin-ui]"))) {
      link.remove();
    }
  }
}
