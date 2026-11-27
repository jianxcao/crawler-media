wasmtime::component::bindgen!({
    path: "../plugin-api/wit",
    world: "hook-plugin",
});

pub mod task_world {
    wasmtime::component::bindgen!({
        path: "../plugin-api/wit",
        world: "task-plugin",
        with: {
            "crawler:plugin/types@1.0.0": crate::bindings::crawler::plugin::types,
            "crawler:plugin/runtime@1.0.0": crate::bindings::crawler::plugin::runtime,
            "crawler:plugin/clock@1.0.0": crate::bindings::crawler::plugin::clock,
            "crawler:plugin/config@1.0.0": crate::bindings::crawler::plugin::config,
            "crawler:plugin/log@1.0.0": crate::bindings::crawler::plugin::log,
            "crawler:plugin/kv@1.0.0": crate::bindings::crawler::plugin::kv,
            "crawler:plugin/blob@1.0.0": crate::bindings::crawler::plugin::blob,
            "crawler:plugin/site@1.0.0": crate::bindings::crawler::plugin::site,
            "crawler:plugin/subscribe@1.0.0": crate::bindings::crawler::plugin::subscribe,
            "crawler:plugin/library@1.0.0": crate::bindings::crawler::plugin::library,
            "crawler:plugin/download@1.0.0": crate::bindings::crawler::plugin::download,
            "crawler:plugin/history@1.0.0": crate::bindings::crawler::plugin::history,
            "crawler:plugin/catalog@1.0.0": crate::bindings::crawler::plugin::catalog,
            "crawler:plugin/operations@1.0.0": crate::bindings::crawler::plugin::operations,
            "crawler:plugin/http@1.0.0": crate::bindings::crawler::plugin::http,
        },
    });
}

pub mod endpoint_world {
    wasmtime::component::bindgen!({
        path: "../plugin-api/wit",
        world: "endpoint-plugin",
        with: {
            "crawler:plugin/types@1.0.0": crate::bindings::crawler::plugin::types,
            "crawler:plugin/runtime@1.0.0": crate::bindings::crawler::plugin::runtime,
            "crawler:plugin/clock@1.0.0": crate::bindings::crawler::plugin::clock,
            "crawler:plugin/config@1.0.0": crate::bindings::crawler::plugin::config,
            "crawler:plugin/log@1.0.0": crate::bindings::crawler::plugin::log,
            "crawler:plugin/kv@1.0.0": crate::bindings::crawler::plugin::kv,
            "crawler:plugin/blob@1.0.0": crate::bindings::crawler::plugin::blob,
            "crawler:plugin/site@1.0.0": crate::bindings::crawler::plugin::site,
            "crawler:plugin/subscribe@1.0.0": crate::bindings::crawler::plugin::subscribe,
            "crawler:plugin/library@1.0.0": crate::bindings::crawler::plugin::library,
            "crawler:plugin/download@1.0.0": crate::bindings::crawler::plugin::download,
            "crawler:plugin/history@1.0.0": crate::bindings::crawler::plugin::history,
            "crawler:plugin/catalog@1.0.0": crate::bindings::crawler::plugin::catalog,
            "crawler:plugin/operations@1.0.0": crate::bindings::crawler::plugin::operations,
            "crawler:plugin/http@1.0.0": crate::bindings::crawler::plugin::http,
            "crawler:plugin/commands@1.0.0": crate::bindings::task_world::crawler::plugin::commands,
            "crawler:plugin/boost@1.0.0": crate::bindings::task_world::crawler::plugin::boost,
            "crawler:plugin/indexer-cache@1.0.0": crate::bindings::task_world::crawler::plugin::indexer_cache,
        },
    });
}

pub mod indexer_world {
    wasmtime::component::bindgen!({
        path: "../plugin-api/wit",
        world: "indexer-plugin",
        with: {
            "crawler:plugin/types@1.0.0": crate::bindings::crawler::plugin::types,
            "crawler:plugin/runtime@1.0.0": crate::bindings::crawler::plugin::runtime,
            "crawler:plugin/clock@1.0.0": crate::bindings::crawler::plugin::clock,
            "crawler:plugin/config@1.0.0": crate::bindings::crawler::plugin::config,
            "crawler:plugin/log@1.0.0": crate::bindings::crawler::plugin::log,
            "crawler:plugin/kv@1.0.0": crate::bindings::crawler::plugin::kv,
            "crawler:plugin/blob@1.0.0": crate::bindings::crawler::plugin::blob,
            "crawler:plugin/site@1.0.0": crate::bindings::crawler::plugin::site,
            "crawler:plugin/subscribe@1.0.0": crate::bindings::crawler::plugin::subscribe,
            "crawler:plugin/library@1.0.0": crate::bindings::crawler::plugin::library,
            "crawler:plugin/download@1.0.0": crate::bindings::crawler::plugin::download,
            "crawler:plugin/history@1.0.0": crate::bindings::crawler::plugin::history,
            "crawler:plugin/catalog@1.0.0": crate::bindings::crawler::plugin::catalog,
            "crawler:plugin/operations@1.0.0": crate::bindings::crawler::plugin::operations,
            "crawler:plugin/http@1.0.0": crate::bindings::crawler::plugin::http,
        },
    });
}

pub mod maintenance_world {
    wasmtime::component::bindgen!({
        path: "../plugin-api/wit",
        world: "maintenance-plugin",
        with: {
            "crawler:plugin/types@1.0.0": crate::bindings::crawler::plugin::types,
            "crawler:plugin/runtime@1.0.0": crate::bindings::crawler::plugin::runtime,
            "crawler:plugin/clock@1.0.0": crate::bindings::crawler::plugin::clock,
            "crawler:plugin/config@1.0.0": crate::bindings::crawler::plugin::config,
            "crawler:plugin/log@1.0.0": crate::bindings::crawler::plugin::log,
            "crawler:plugin/kv@1.0.0": crate::bindings::crawler::plugin::kv,
            "crawler:plugin/blob@1.0.0": crate::bindings::crawler::plugin::blob,
            "crawler:plugin/site@1.0.0": crate::bindings::crawler::plugin::site,
            "crawler:plugin/subscribe@1.0.0": crate::bindings::crawler::plugin::subscribe,
            "crawler:plugin/library@1.0.0": crate::bindings::crawler::plugin::library,
            "crawler:plugin/download@1.0.0": crate::bindings::crawler::plugin::download,
            "crawler:plugin/history@1.0.0": crate::bindings::crawler::plugin::history,
            "crawler:plugin/catalog@1.0.0": crate::bindings::crawler::plugin::catalog,
            "crawler:plugin/operations@1.0.0": crate::bindings::crawler::plugin::operations,
            "crawler:plugin/http@1.0.0": crate::bindings::crawler::plugin::http,
            "crawler:plugin/browser@1.0.0": crate::bindings::indexer_world::crawler::plugin::browser,
        },
    });
}
