use axum::http::StatusCode;
use serde_json::{Value, json};

use crate::post_write;
use crate::support::plugin_http_support::{HttpEnv, assert_err, assert_ok, pkg};

fn build_req_and_opt_task_zip(id: &str) -> Vec<u8> {
    let manifest = format!(
        r#"manifest_version = 1
id = "{id}"
name = "Task Input Plugin"
version = "1.0.0"
api = "1.0.0"
license = "MIT"
authors = ["Test Author"]
description = "Task run effective input test"
config_schema = "schemas/config.schema.json"
state_schema = 1

[[components]]
id = "worker"
entry = "components/worker.wasm"
world = "crawler:plugin/task-plugin@1.0.0"
permissions = ["kv"]

[[components.tasks]]
id = "req-sync"
input_schema = "schemas/req.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300

[[components.tasks]]
id = "opt-sync"
input_schema = "schemas/opt.schema.json"
[components.tasks.schedule]
kind = "interval"
seconds = 300
"#
    );
    let cfg_schema = pkg::sample_schema(false, None);
    let req_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"required":["target"],"properties":{"target":{"type":"string","minLength":1}}}"#;
    let opt_schema = r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","type":"object","additionalProperties":false,"properties":{"note":{"type":"string"}}}"#;
    pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("schemas/config.schema.json", cfg_schema.as_bytes()),
        ("schemas/req.schema.json", req_schema.as_bytes()),
        ("schemas/opt.schema.json", opt_schema.as_bytes()),
        ("components/worker.wasm", pkg::task_wasm()),
    ])
}

#[tokio::test]
async fn slice_d_r4_item4_run_task_effective_input_default_object_and_stale_override_cleanup() {
    let env = HttpEnv::new().await;
    env.enable_dev_mode().await;
    let store = env.state.store();

    let pid = "org.example.r4-task-in";
    let zip = build_req_and_opt_task_zip(pid);
    let installed = env.install_pkg(&zip, None, json!([])).await;
    env.enable_plugin(pid, installed["revision"].as_i64().unwrap(), "r4-ti-en")
        .await;

    // Clear seeded task bindings to exercise the None branch in begin_and_claim_run_task too.
    store
        .with_writer(store::Db::App, |c| {
            c.execute("DELETE FROM plugin_task_bindings WHERE plugin_id=?1", [pid])?;
            Ok(())
        })
        .unwrap();

    // 1. Unconfigured required-field task rejects `{}`, `{"input": null}`, and `{"input": {}}`!
    for (idx, body) in [json!({}), json!({ "input": null }), json!({ "input": {} })]
        .into_iter()
        .enumerate()
    {
        let (st, b) = post_write(
            &env,
            format!("/api/v1/plugins/{pid}/tasks/req-sync/run"),
            0,
            format!("r4-ti-bad-{idx}"),
            body,
        )
        .await;
        assert_err(st, &b, StatusCode::BAD_REQUEST, "plugin.config_invalid");
    }

    // 2. Unconfigured optional-field task with `{"input": null}` saves `{}` (NEVER null) in public_config!
    let (st_opt, b_opt) = post_write(
        &env,
        format!("/api/v1/plugins/{pid}/tasks/opt-sync/run"),
        0,
        "r4-ti-opt-null",
        json!({ "input": null }),
    )
    .await;
    assert_ok(st_opt, &b_opt, StatusCode::ACCEPTED);
    let opt_binding = store
        .get_plugin_task_binding(pid, "opt-sync")
        .unwrap()
        .unwrap();
    assert_eq!(
        opt_binding.definition.public_config,
        json!({}),
        "None branch must save empty JSON object, never Value::Null"
    );

    assert_configured_task_and_queued_override_cleanup(&env, pid).await;
}

async fn assert_configured_task_and_queued_override_cleanup(env: &HttpEnv, pid: &str) {
    let store = env.state.store();
    let (st_up, b_up) = post_write(
        env,
        format!("/api/v1/plugins/{pid}/tasks/req-sync/update"),
        0,
        "r4-ti-cfg",
        json!({
            "enabled": true,
            "schedule": { "interval_secs": 300 },
            "config": { "target": "saved-default" }
        }),
    )
    .await;
    assert_ok(st_up, &b_up, StatusCode::ACCEPTED);
    let binding = store
        .get_plugin_task_binding(pid, "req-sync")
        .unwrap()
        .unwrap();
    let def_rev = binding.definition_revision;

    // 3. `POST .../run` with `{"input": {}}` succeeds because saved public_config satisfies required fields!
    let (st_r1, b_r1) = post_write(
        env,
        format!("/api/v1/plugins/{pid}/tasks/req-sync/run"),
        def_rev,
        "r4-ti-empty-obj",
        json!({ "input": {} }),
    )
    .await;
    assert_ok(st_r1, &b_r1, StatusCode::ACCEPTED);

    // 4. `POST .../run` with override sets `"input"` on Queued job, then subsequent `{}` clears `"input"`!
    let (st_r2, b_r2) = post_write(
        env,
        format!("/api/v1/plugins/{pid}/tasks/req-sync/run"),
        def_rev,
        "r4-ti-with-ov",
        json!({ "input": { "target": "one-shot-override" } }),
    )
    .await;
    assert_ok(st_r2, &b_r2, StatusCode::ACCEPTED);

    let read_queued_payload = || -> Value {
        let jobs_writer = env.state.jobs();
        let queue = jobs_writer.acquire().unwrap();
        let bid = domain::PluginTaskBindingId::from_uuid(binding.binding_id);
        let def = queue
            .def_for_identity(jobs::JobIdentity::plugin_task(bid))
            .unwrap()
            .unwrap();
        let live = queue.get_live_child(def.id).unwrap().unwrap();
        assert_eq!(live.status, jobs::JobStatus::Queued);
        serde_json::from_str(&live.payload).unwrap()
    };
    assert_eq!(
        read_queued_payload()["input"],
        json!({ "target": "one-shot-override" })
    );

    let (st_r3, b_r3) = post_write(
        env,
        format!("/api/v1/plugins/{pid}/tasks/req-sync/run"),
        def_rev,
        "r4-ti-clear-ov",
        json!({}),
    )
    .await;
    assert_ok(st_r3, &b_r3, StatusCode::ACCEPTED);
    assert!(
        read_queued_payload().get("input").is_none(),
        "subsequent non-override run_task must remove stale `input` from Queued job payload"
    );
}
