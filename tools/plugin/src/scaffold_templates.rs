use std::fmt::Write;

pub const WIT_TYPES: &str = include_str!("../../../crates/plugin-api/wit/types.wit");
pub const WIT_SERVICES: &str = include_str!("../../../crates/plugin-api/wit/services.wit");
pub const WIT_WORLDS: &str = include_str!("../../../crates/plugin-api/wit/worlds.wit");
const SDK_CARGO_LOCK: &str = include_str!("../../../sdk/Cargo.lock");

pub const GITIGNORE: &str = "/target\n/*.cmplugin\n/*.minisig\n/components/*.wasm\n/ui/node_modules\n/ui/assets\n/ui/asset-manifest.json\n";

pub const DEFAULT_CONFIG_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "properties": {
    "greeting": {
      "type": "string",
      "title": "Greeting",
      "default": "hello"
    }
  }
}
"#;

pub const EMPTY_INPUT_SCHEMA: &str = r#"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "additionalProperties": false,
  "properties": {}
}
"#;

pub fn build_cargo_toml(crate_name: &str) -> String {
    format!(
        r#"[package]
name = "{crate_name}"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = {{ version = "=0.63.0", default-features = false, features = ["macros", "realloc"] }}
dlmalloc = {{ version = "=0.2.13", features = ["global"] }}

[profile.release]
panic = "abort"
opt-level = "s"
strip = true
lto = true
"#
    )
}

pub fn build_cargo_lock(crate_name: &str) -> String {
    let mut out = String::from(
        "# Auto-generated locked dependency graph for offline WASM plugin builds.\nversion = 4\n\n",
    );
    let _ = writeln!(
        out,
        "[[package]]\nname = \"{crate_name}\"\nversion = \"0.1.0\"\ndependencies = [\n \"dlmalloc\",\n \"wit-bindgen\",\n]\n"
    );

    let mut current_block = String::new();
    let flush_block = |block: &str, dest: &mut String| {
        if block.contains("source = \"registry+") {
            dest.push_str(block);
            if !dest.ends_with("\n\n") {
                dest.push('\n');
            }
        }
    };
    for line in SDK_CARGO_LOCK.lines() {
        if line == "[[package]]" {
            flush_block(&current_block, &mut out);
            current_block.clear();
        }
        if !current_block.is_empty() || line == "[[package]]" {
            current_block.push_str(line);
            current_block.push('\n');
        }
    }
    flush_block(&current_block, &mut out);
    out
}

pub fn build_guest_lib_rs(world_short: &str) -> String {
    let world_body = match world_short {
        "hook-plugin" => HOOK_IMPL,
        "task-plugin" => TASK_IMPL,
        "endpoint-plugin" => ENDPOINT_IMPL,
        "indexer-plugin" => INDEXER_IMPL,
        "maintenance-plugin" => MAINTENANCE_IMPL,
        _ => "",
    };

    format!(
        r#"#![cfg_attr(target_arch = "wasm32", no_std)]
extern crate alloc;

#[cfg(target_arch = "wasm32")]
mod wasm_rt {{
    #[global_allocator]
    static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

    // SAFETY: the C ABI caller supplies two valid readable regions of `len` bytes.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn memcmp(
        a: *const core::ffi::c_void,
        b: *const core::ffi::c_void,
        len: usize,
    ) -> i32 {{
        let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
        for index in 0..len {{
            // SAFETY: the caller's regions cover every index in this loop.
            let (a, b) = unsafe {{ (a.add(index).read_volatile(), b.add(index).read_volatile()) }};
            if a != b {{
                return i32::from(a) - i32::from(b);
            }}
        }}
        0
    }}

    // SAFETY: the canonical ABI guarantees valid allocation metadata.
    #[unsafe(no_mangle)]
    unsafe extern "C" fn cabi_realloc(
        ptr: *mut u8,
        old: usize,
        align: usize,
        new: usize,
    ) -> *mut u8 {{
        if old == 0 && new == 0 {{
            return align as *mut u8;
        }}
        let layout = match core::alloc::Layout::from_size_align(old.max(new), align) {{
            Ok(layout) => layout,
            Err(_) => core::arch::wasm32::unreachable(),
        }};
        // SAFETY: layout is validated and `ptr` came from a prior canonical ABI alloc.
        let result = unsafe {{
            if old == 0 {{
                alloc::alloc::alloc(layout)
            }} else {{
                let old_layout = core::alloc::Layout::from_size_align_unchecked(old, align);
                alloc::alloc::realloc(ptr, old_layout, new)
            }}
        }};
        if result.is_null() {{
            core::arch::wasm32::unreachable();
        }}
        result
    }}

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo<'_>) -> ! {{
        core::arch::wasm32::unreachable()
    }}
}}

pub fn plugin_greeting_prefix() -> &'static str {{
    "crawler-media"
}}

#[cfg(target_arch = "wasm32")]
mod bindings {{
    use alloc::string::String;
    use alloc::vec::Vec;

    wit_bindgen::generate!({{
        path: "./wit",
        world: "{world_short}",
        pub_export_macro: true,
    }});

    use crawler::plugin::types;
    use exports::crawler::plugin::lifecycle;

    struct Plugin;

    impl lifecycle::Guest for Plugin {{
        fn validate(_config_schema: String, _state_schema: u32) -> Result<(), types::ApiError> {{
            Ok(())
        }}

        fn health() -> Result<lifecycle::HealthState, types::ApiError> {{
            Ok(lifecycle::HealthState::Healthy)
        }}
    }}

{world_body}

    export!(Plugin with_types_in crate::bindings);
}}

#[cfg(test)]
mod tests {{
    #[test]
    fn offline_contract_greeting_is_non_empty() {{
        assert_eq!(super::plugin_greeting_prefix(), "crawler-media");
    }}
}}
"#
    )
}

const HOOK_IMPL: &str = r#"    impl exports::crawler::plugin::hooks::Guest for Plugin {
        fn filter(_input: types::FilterInput) -> Result<Vec<types::Patch>, types::ApiError> {
            Ok(Vec::new())
        }

        fn gate(_input: types::GateInput) -> Result<types::GateDecision, types::ApiError> {
            Ok(types::GateDecision::Abstain)
        }

        fn notify(_event: types::Event) -> Result<(), types::ApiError> {
            Ok(())
        }
    }"#;

const TASK_IMPL: &str = r#"    impl exports::crawler::plugin::tasks::Guest for Plugin {
        fn run(_input: exports::crawler::plugin::tasks::TaskInput) -> Result<exports::crawler::plugin::tasks::TaskOutcome, types::ApiError> {
            Ok(exports::crawler::plugin::tasks::TaskOutcome {
                partial: false,
                progress: 100,
                operation_ids: Vec::new(),
                summary: String::from("completed"),
            })
        }
    }"#;

const ENDPOINT_IMPL: &str = r#"    impl exports::crawler::plugin::endpoints::Guest for Plugin {
        fn handle(_request: exports::crawler::plugin::endpoints::EndpointRequest) -> Result<exports::crawler::plugin::endpoints::EndpointResponse, types::ApiError> {
            Ok(exports::crawler::plugin::endpoints::EndpointResponse {
                status: 200,
                headers: Vec::new(),
                body: b"{\"ok\":true}".to_vec(),
            })
        }
    }"#;

const INDEXER_IMPL: &str = r#"    impl exports::crawler::plugin::indexer::Guest for Plugin {
        fn describe() -> Vec<types::Descriptor> {
            alloc::vec![types::Descriptor {
                id: String::from("default-indexer"),
                name: String::from("Default Indexer"),
                kind: String::from("pt"),
                capabilities: alloc::vec![String::from("search")],
            }]
        }

        fn search(_query: types::SearchQuery) -> Result<types::TorrentBatch, types::ApiError> {
            Ok(types::TorrentBatch {
                items: Vec::new(),
                next_cursor: None,
                has_more: false,
            })
        }

        fn rss(_cursor: Option<String>) -> Result<types::TorrentBatch, types::ApiError> {
            Ok(types::TorrentBatch {
                items: Vec::new(),
                next_cursor: None,
                has_more: false,
            })
        }

        fn detail(_torrent_ref: String) -> Result<types::TorrentRaw, types::ApiError> {
            Err(types::ApiError {
                code: types::ErrorCode::Unsupported,
                message: String::from("detail not implemented"),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            })
        }

        fn resolve_download(_download_ref: String) -> Result<types::HttpRequest, types::ApiError> {
            Err(types::ApiError {
                code: types::ErrorCode::Unsupported,
                message: String::from("resolve-download not implemented"),
                detail: None,
                retry_after_ms: None,
                operation_id: None,
            })
        }

        fn user_stats() -> Result<types::StatsView, types::ApiError> {
            Ok(types::StatsView {
                uploaded: None,
                downloaded: None,
            })
        }

        fn categories() -> Result<Vec<types::Category>, types::ApiError> {
            Ok(Vec::new())
        }
    }"#;

const MAINTENANCE_IMPL: &str = r#"    impl exports::crawler::plugin::maintenance::Guest for Plugin {
        fn run(
            _action: types::MaintenanceAction,
            job_id: String,
        ) -> Result<types::MaintenanceOutcome, types::ApiError> {
            Ok(types::MaintenanceOutcome {
                state: types::MaintenanceState::Completed,
                evidence: job_id,
                session_capture: None,
            })
        }
    }"#;

fn encode_toml_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\u{0008}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{000C}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if c.is_control() => {
                let code = c as u32;
                if code <= 0xFFFF {
                    let _ = write!(out, "\\u{code:04X}");
                } else {
                    let _ = write!(out, "\\U{code:08X}");
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

pub fn build_manifest_toml(
    id: &str,
    name: &str,
    world_short: Option<&str>,
    with_ui: bool,
    remote_name: &str,
) -> String {
    let id_toml = encode_toml_basic_string(id);
    let name_toml = encode_toml_basic_string(name);
    let cred_line = match world_short {
        Some("indexer-plugin" | "maintenance-plugin") => {
            "credential_binding = \"invocation-site\"\n"
        }
        _ => "",
    };
    let component_section = component_toml_section(world_short);
    let ui_section = if with_ui {
        ui_toml_section(remote_name)
    } else {
        String::new()
    };

    format!(
        r#"manifest_version = 1
id = {id_toml}
name = {name_toml}
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Plugin Author"]
description = "Scaffolded crawler-media plugin"
config_schema = "schemas/config.schema.json"
state_schema = 1
{cred_line}{component_section}{ui_section}"#
    )
}

fn component_toml_section(world_short: Option<&str>) -> &'static str {
    match world_short {
        Some("hook-plugin") => {
            r#"
[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/hook-plugin@1.0.0"
permissions = []

[[components.hooks]]
id = "search.after"
priority = 100
"#
        }
        Some("task-plugin") => {
            r#"
[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = []

[[components.tasks]]
id = "sync"
input_schema = "schemas/task-input.schema.json"

[components.tasks.schedule]
kind = "interval"
seconds = 3600
"#
        }
        Some("endpoint-plugin") => {
            r#"
[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/endpoint-plugin@1.0.0"
permissions = ["endpoints"]

[[components.endpoints]]
id = "status"
method = "GET"
path = "/status"
auth = "admin"
input_schema = "schemas/endpoint-input.schema.json"
capabilities = []
"#
        }
        Some("indexer-plugin") => {
            r#"
[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/indexer-plugin@1.0.0"
permissions = ["indexer:provide"]

[[components.indexers]]
id = "default-indexer"
name = "Default Indexer"
kind = "pt"
capabilities = ["search"]
site_schema = "schemas/site.schema.json"
"#
        }
        Some("maintenance-plugin") => {
            r#"
[[components]]
id = "main"
entry = "components/main.wasm"
world = "crawler:plugin/maintenance-plugin@1.0.0"
permissions = ["maintenance:provide"]

[[components.maintenance]]
id = "default-maintenance"
actions = ["verify-session", "check-in"]
"#
        }
        _ => "",
    }
}

fn ui_toml_section(remote_name: &str) -> String {
    let remote_name_toml = encode_toml_basic_string(remote_name);
    format!(
        r#"
[ui]
kind = "federation-react"
contract = "1.0.0"
remote_name = {remote_name_toml}
entry = "ui/assets/remoteEntry.js"
asset_manifest = "ui/asset-manifest.json"
react = "19.3.0"
react_dom = "19.3.0"
capabilities = ["read:sites"]

[[ui.surfaces]]
id = "config"
kind = "config"
expose = "./Config"
title = "Settings"
icon = "sliders"

[[ui.surfaces]]
id = "page"
kind = "page"
expose = "./Page"
title = "Overview"
icon = "puzzle"

[[ui.surfaces]]
id = "app"
kind = "app"
expose = "./AppPage"
title = "Workspace"
icon = "activity"

[[ui.surfaces]]
id = "dashboard"
kind = "dashboard"
expose = "./Dashboard"
title = "Summary"
icon = "gauge"
"#
    )
}

pub fn build_ui_package_json(pkg_name: &str) -> String {
    format!(
        r#"{{
  "name": "{pkg_name}-ui",
  "private": true,
  "version": "1.0.0",
  "type": "module",
  "scripts": {{
    "build": "vite build --config vite.config.mjs"
  }},
  "dependencies": {{
    "@crawler-media/plugin-ui-kit": "1.0.0",
    "@crawler-media/plugin-ui-sdk": "1.0.0",
    "react": "19.3.0",
    "react-dom": "19.3.0"
  }},
  "devDependencies": {{
    "@module-federation/vite": "^1.24.1",
    "@vitejs/plugin-react": "^5.0.0",
    "vite": "7.3.6"
  }}
}}
"#
    )
}

pub const UI_TSCONFIG_JSON: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["DOM", "DOM.Iterable", "ESNext"],
    "strict": true,
    "noEmit": true,
    "module": "ESNext",
    "moduleResolution": "bundler",
    "isolatedModules": true,
    "skipLibCheck": true,
    "jsx": "react-jsx"
  },
  "include": ["src/**/*.ts", "src/**/*.tsx"]
}
"#;

pub fn build_ui_vite_config_mjs(remote_name: &str) -> String {
    format!(
        r#"import path from "node:path";
import {{ fileURLToPath, pathToFileURL }} from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const webRoot = process.env.CM_WEB_ROOT;

async function resolveDep(pkgSubpath, fallbackSpec) {{
  if (webRoot) {{
    return import(pathToFileURL(path.join(webRoot, "node_modules", pkgSubpath)).href);
  }}
  return import(fallbackSpec);
}}

const {{ defineConfig }} = await resolveDep("vite/dist/node/index.js", "vite");
const {{ default: react }} = await resolveDep("@vitejs/plugin-react/dist/index.js", "@vitejs/plugin-react");
const {{ federation }} = await resolveDep("@module-federation/vite/lib/index.js", "@module-federation/vite");

const resolveAlias = webRoot
  ? {{
      react: path.join(webRoot, "node_modules/react"),
      "react-dom": path.join(webRoot, "node_modules/react-dom"),
      "@crawler-media/plugin-ui-sdk": path.join(webRoot, "../sdk/plugin-ui/src/index.ts"),
      "@crawler-media/plugin-ui-kit": path.join(webRoot, "../sdk/plugin-ui-kit/src/index.tsx"),
    }}
  : {{}};

export default defineConfig({{
  root: here,
  base: "./",
  logLevel: "silent",
  resolve: {{
    alias: resolveAlias,
  }},
  plugins: [
    react(),
    federation({{
      name: {remote_name_json},
      filename: "assets/remoteEntry.js",
      exposes: {{
        "./Config": "./src/Config.tsx",
        "./Page": "./src/Page.tsx",
        "./AppPage": "./src/AppPage.tsx",
        "./Dashboard": "./src/Dashboard.tsx",
      }},
      dts: false,
      manifest: false,
      shared: {{
        react: {{ singleton: true, requiredVersion: "19.3.0", import: false, suppressMissingImportWarning: true }},
        "react-dom": {{ singleton: true, requiredVersion: "19.3.0", import: false, suppressMissingImportWarning: true }},
        "react/jsx-runtime": {{ singleton: true, requiredVersion: "19.3.0", import: false, suppressMissingImportWarning: true }},
        "@crawler-media/plugin-ui-sdk": {{ singleton: true, requiredVersion: "1.0.0", import: false, suppressMissingImportWarning: true }},
        "@crawler-media/plugin-ui-kit": {{ singleton: true, requiredVersion: "1.0.0", import: false, suppressMissingImportWarning: true }},
      }},
    }}),
  ],
  build: {{
    outDir: ".",
    emptyOutDir: false,
    target: "es2022",
    cssCodeSplit: false,
    minify: false,
    sourcemap: false,
    rollupOptions: {{
      input: "./src/entry.ts",
    }},
  }},
}});
"#,
        remote_name_json =
            serde_json::to_string(remote_name).unwrap_or_else(|_| "\"cm_plugin\"".to_string())
    )
}

pub const UI_ENTRY_TS: &str = "export {};\n";

pub const UI_CONFIG_TSX: &str = r#"import * as React from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { ActionBar, Card, FormField } from "@crawler-media/plugin-ui-kit";

export default function ConfigSurface({ config }: PluginSurfaceProps) {
  const [greeting, setGreeting] = React.useState(
    String(config?.values.greeting ?? "hello"),
  );

  return (
    <Card title="Plugin Configuration">
      <FormField label="Greeting" htmlFor="cm-greeting">
        <input
          id="cm-greeting"
          value={greeting}
          disabled={config ? !config.editable : false}
          onChange={(e) => {
            const next = e.target.value;
            setGreeting(next);
            config?.onDraftChange({ ...config.values, greeting: next });
          }}
        />
      </FormField>
      {config ? (
        <ActionBar
          items={[
            {
              id: "save",
              label: "Save",
              tone: "primary",
              disabled: !config.editable,
              onClick: () => void config.save({ ...config.values, greeting }),
            },
          ]}
        />
      ) : null}
    </Card>
  );
}
"#;

pub const UI_PAGE_TSX: &str = r#"import * as React from "react";
import { usePluginTheme, type PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { Card, PageHeader, StatBadge, StatusPill } from "@crawler-media/plugin-ui-kit";

export default function PageSurface({ host, surfaceId }: PluginSurfaceProps) {
  const theme = usePluginTheme(host);
  return (
    <Card>
      <PageHeader
        title="Plugin Surface"
        subtitle={`Surface: ${surfaceId}`}
        badge={<StatusPill label={theme} tone="info" />}
      />
      <StatBadge label="Contract" value="1.0.0" tone="ok" />
    </Card>
  );
}
"#;

pub const UI_APP_PAGE_TSX: &str = r#"import * as React from "react";
import { usePluginQuery, type PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { Card, DataTable, EmptyState, PageHeader } from "@crawler-media/plugin-ui-kit";

export default function AppPageSurface({ host }: PluginSurfaceProps) {
  const sites = usePluginQuery("sites.list", { limit: 20 }, { host });
  const rows = sites.data?.items ?? [];

  return (
    <Card>
      <PageHeader title="Plugin Workspace" />
      {rows.length === 0 ? (
        <EmptyState title="No sites available" />
      ) : (
        <DataTable
          columns={[
            { key: "id", header: "ID", render: (r) => r.id },
            { key: "name", header: "Site", render: (r) => r.name },
          ]}
          rows={rows}
          getRowKey={(r) => r.id}
        />
      )}
    </Card>
  );
}
"#;

pub const UI_DASHBOARD_TSX: &str = r#"import * as React from "react";
import { usePluginTheme, type PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { Card, StatBadge } from "@crawler-media/plugin-ui-kit";

export default function DashboardSurface({ host }: PluginSurfaceProps) {
  const theme = usePluginTheme(host);
  return (
    <Card title="Plugin Status">
      <StatBadge label="Theme" value={theme} tone="ok" />
    </Card>
  );
}
"#;
