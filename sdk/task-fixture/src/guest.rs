use alloc::{string::String, vec, vec::Vec};

wit_bindgen::generate!({
    path: "../../crates/plugin-api/wit",
    world: "task-plugin",
    pub_export_macro: true,
});

use crawler::plugin::{blob, boost, commands, config, kv, log, operations, runtime, types};
use exports::crawler::plugin::{lifecycle, tasks};

#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

// SAFETY: the C ABI caller supplies two valid readable regions of `len` bytes.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(
    a: *const core::ffi::c_void,
    b: *const core::ffi::c_void,
    len: usize,
) -> i32 {
    let (a, b) = (a.cast::<u8>(), b.cast::<u8>());
    for index in 0..len {
        // SAFETY: the caller's regions cover every index in this loop.
        let (a, b) = unsafe { (a.add(index).read_volatile(), b.add(index).read_volatile()) };
        if a != b {
            return i32::from(a) - i32::from(b);
        }
    }
    0
}

// SAFETY: the canonical ABI guarantees a valid prior allocation and alignment.
#[unsafe(no_mangle)]
unsafe extern "C" fn cabi_realloc(ptr: *mut u8, old: usize, align: usize, new: usize) -> *mut u8 {
    if old == 0 && new == 0 {
        return align as *mut u8;
    }
    let layout = match core::alloc::Layout::from_size_align(old.max(new), align) {
        Ok(layout) => layout,
        Err(_) => core::arch::wasm32::unreachable(),
    };
    // SAFETY: validated layout and canonical ABI ownership of the prior allocation.
    let result = unsafe {
        if old == 0 {
            alloc::alloc::alloc(layout)
        } else {
            let old_layout = core::alloc::Layout::from_size_align_unchecked(old, align);
            alloc::alloc::realloc(ptr, old_layout, new)
        }
    };
    if result.is_null() {
        core::arch::wasm32::unreachable();
    }
    result
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    core::arch::wasm32::unreachable()
}

struct TaskFixture;

fn err(code: types::ErrorCode, msg: &str) -> types::ApiError {
    types::ApiError {
        code,
        message: String::from(msg),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

impl lifecycle::Guest for TaskFixture {
    fn validate(config_schema: String, _state_schema: u32) -> Result<(), types::ApiError> {
        if config_schema.contains("reject") {
            return Err(err(types::ErrorCode::InvalidArgument, "rejected by guest"));
        }
        if config_schema.contains("wrong-context-boost") {
            return match boost::snapshot(&[], &[]) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(error) => Err(error),
            };
        }
        if config_schema.contains("wrong-context-submit") {
            return match commands::submit(&types::CommandRequest {
                action: types::Command::SiteMaintenance((
                    String::from("global"),
                    types::MaintenanceAction::CheckIn,
                )),
                idempotency_key: String::from("wrong-context-submit"),
                expected_revision: None,
            }) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(error) => Err(error),
            };
        }
        if config_schema.contains("wrong-context-kv-put") {
            return match kv::put("k", b"v", None) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(error) => Err(error),
            };
        }
        if config_schema.contains("wrong-context-kv-delete") {
            return match kv::delete("k", None) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(error) => Err(error),
            };
        }
        if config_schema.contains("wrong-context-blob-put") {
            return match blob::put("b", b"v", None) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(error) => Err(error),
            };
        }
        if config_schema.contains("wrong-context-blob-delete") {
            return match blob::delete("b", None) {
                Ok(_) => Err(err(types::ErrorCode::Conflict, "expected wrong-context")),
                Err(error) => Err(error),
            };
        }
        if config_schema.contains("assert-default-mode-ok") {
            let cfg = config::get()?;
            if !cfg.validated_json.contains("\"mode\":\"ok\"") {
                return Err(err(
                    types::ErrorCode::InvalidArgument,
                    "expected materialized default mode=ok",
                ));
            }
            log::write(log::Level::Info, "validate-default-ok", &[])?;
            return Ok(());
        }
        if config_schema.contains("read-config-and-log") {
            let _ = config::get()?;
            log::write(log::Level::Info, "validate-ok", &[])?;
            return Ok(());
        }
        Ok(())
    }

    fn health() -> Result<lifecycle::HealthState, types::ApiError> {
        Ok(lifecycle::HealthState::Healthy)
    }
}

impl tasks::Guest for TaskFixture {
    fn run(input: tasks::TaskInput) -> Result<tasks::TaskOutcome, types::ApiError> {
        match input.validated_input.as_str() {
            "block-kv" => {
                let _ = kv::get("block")?;
                Ok(tasks::TaskOutcome {
                    partial: false,
                    progress: 100,
                    operation_ids: Vec::new(),
                    summary: String::from("blocked-done"),
                })
            }
            "loop" => loop {
                core::hint::spin_loop();
            },
            "boost-snapshot" => run_boost_snapshot(),
            "downloader-commands" => run_downloader_commands(),
            "submit-and-state" | _ => run_submit_and_state(),
        }
    }
}

fn run_boost_snapshot() -> Result<tasks::TaskOutcome, types::ApiError> {
    let snap = boost::snapshot(&[String::from("pool:1")], &[String::from("dl-1")])?;
    let disk = snap.disk.available_bytes.unwrap_or(0);
    Ok(tasks::TaskOutcome {
        partial: false,
        progress: 100,
        operation_ids: Vec::new(),
        summary: alloc::format!(
            "used={};reserved={};retained={};disk={}",
            snap.used_bytes,
            snap.reserved_bytes,
            snap.retained_file_bytes,
            disk
        ),
    })
}

fn run_downloader_commands() -> Result<tasks::TaskOutcome, types::ApiError> {
    let opts = types::DownloadOptions {
        relative_directory: Some(String::from("boost")),
        category: Some(String::from("pt")),
        tags: vec![String::from("plugin:builtin.boost:auto")],
        paused: true,
        upload_limit: Some(1024),
        download_limit: None,
        sequential: false,
        first_last_piece: false,
    };
    let a1 = commands::submit(&types::CommandRequest {
        action: types::Command::DownloaderAdd((String::from("dl-1"), String::from("ref-1"), opts)),
        idempotency_key: String::from("dl-add-1"),
        expected_revision: None,
    })?;
    let a2 = commands::submit(&types::CommandRequest {
        action: types::Command::DownloaderControl((
            types::OwnedRevision {
                handle: String::from("task-1"),
                revision: 2,
            },
            types::TaskControl::Pause,
        )),
        idempotency_key: String::from("dl-ctrl-1"),
        expected_revision: None,
    })?;
    let a3 = commands::submit(&types::CommandRequest {
        action: types::Command::DownloaderDeleteTask((
            String::from("dl-1"),
            3,
            String::from("task-1"),
        )),
        idempotency_key: String::from("dl-del-task-1"),
        expected_revision: None,
    })?;
    let a4 = commands::submit(&types::CommandRequest {
        action: types::Command::DownloaderPrepareCleanup((
            vec![types::OwnedRevision {
                handle: String::from("task-1"),
                revision: 3,
            }],
            String::from("quota"),
        )),
        idempotency_key: String::from("dl-prep-1"),
        expected_revision: None,
    })?;
    let a5 = commands::submit(&types::CommandRequest {
        action: types::Command::DownloaderDeleteFiles((
            types::OwnedRevision {
                handle: String::from("task-1"),
                revision: 4,
            },
            String::from("plan-1"),
        )),
        idempotency_key: String::from("dl-del-files-1"),
        expected_revision: None,
    })?;
    Ok(tasks::TaskOutcome {
        partial: false,
        progress: 100,
        operation_ids: vec![
            a1.operation_id,
            a2.operation_id,
            a3.operation_id,
            a4.operation_id,
            a5.operation_id,
        ],
        summary: String::from("downloader-commands-ok"),
    })
}

fn run_submit_and_state() -> Result<tasks::TaskOutcome, types::ApiError> {
    let cfg = config::get()?;
    log::write(
        log::Level::Info,
        "task-fixture-run",
        &[(String::from("task"), String::from("submit-and-state"))],
    )?;
    let etag = kv::put("k/a", b"val-a", None)?;
    let entry =
        kv::get("k/a")?.ok_or_else(|| err(types::ErrorCode::NotFound, "missing written kv"))?;
    if entry.value != b"val-a" || entry.etag != etag {
        return Err(err(types::ErrorCode::Conflict, "kv mismatch"));
    }
    let page = kv::list(
        "k/",
        &types::PageRequest {
            cursor: None,
            limit: 10,
        },
    )?;
    if page.items.len() != 1 {
        return Err(err(types::ErrorCode::Conflict, "kv list count mismatch"));
    }
    kv::put("k/tmp", b"temp", None)?;
    kv::delete("k/tmp", None)?;

    let btag = blob::put("b/a", b"blob-val", None)?;
    let bentry = blob::get("b/a")?;
    if bentry.value != b"blob-val" || bentry.etag != btag {
        return Err(err(types::ErrorCode::Conflict, "blob mismatch"));
    }
    let bpage = blob::list(
        "b/",
        &types::PageRequest {
            cursor: None,
            limit: 10,
        },
    )?;
    if bpage.items.len() != 1 {
        return Err(err(types::ErrorCode::Conflict, "blob list count mismatch"));
    }
    blob::put("b/tmp", b"temp", None)?;
    blob::delete("b/tmp", None)?;

    let site = runtime::current_context()
        .site_id
        .unwrap_or_else(|| String::from("global"));
    let accepted = commands::submit(&types::CommandRequest {
        action: types::Command::SiteMaintenance((site, types::MaintenanceAction::CheckIn)),
        idempotency_key: String::from("task-cmd-1"),
        expected_revision: None,
    })?;
    let op = operations::get(&accepted.operation_id)?;
    let ops = operations::list(&types::PageRequest {
        cursor: None,
        limit: 10,
    })?;
    if !ops.items.iter().any(|item| item.id == op.id) {
        return Err(err(types::ErrorCode::NotFound, "operation missing in list"));
    }
    Ok(tasks::TaskOutcome {
        partial: false,
        progress: 100,
        operation_ids: vec![accepted.operation_id],
        summary: cfg.validated_json,
    })
}

export!(TaskFixture with_types_in crate::guest);
