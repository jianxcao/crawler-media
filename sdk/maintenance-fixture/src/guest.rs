use alloc::{format, string::String, vec::Vec};

wit_bindgen::generate!({
    path: "../../crates/plugin-api/wit",
    world: "maintenance-plugin",
    pub_export_macro: true,
});

use crawler::plugin::{browser, config, http, kv, site, types};
use exports::crawler::plugin::{lifecycle, maintenance};

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

struct MaintenanceFixture;

fn err(code: types::ErrorCode, msg: &str) -> types::ApiError {
    types::ApiError {
        code,
        message: String::from(msg),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

impl lifecycle::Guest for MaintenanceFixture {
    fn validate(config_schema: String, _state_schema: u32) -> Result<(), types::ApiError> {
        if config_schema.contains("reject") {
            return Err(err(types::ErrorCode::InvalidArgument, "rejected by guest"));
        }
        Ok(())
    }

    fn health() -> Result<lifecycle::HealthState, types::ApiError> {
        if let Ok(cfg) = config::get()
            && cfg.validated_json.contains("degrade")
        {
            return Ok(lifecycle::HealthState::Degraded(String::from("degraded")));
        }
        Ok(lifecycle::HealthState::Healthy)
    }
}

impl maintenance::Guest for MaintenanceFixture {
    fn run(
        action: types::MaintenanceAction,
        job_id: String,
    ) -> Result<types::MaintenanceOutcome, types::ApiError> {
        let action_label = match action {
            types::MaintenanceAction::VerifySession => "verify-session",
            types::MaintenanceAction::CheckIn => "check-in",
        };
        match job_id.as_str() {
            "http-checkin" => {
                let page = site::list(&types::PageRequest {
                    cursor: None,
                    limit: 5,
                })?;
                let first_id = page.items.first().map(|s| s.id.clone()).unwrap_or_default();
                let view = site::get(&first_id)?;
                let resp = http::fetch(&types::HttpRequest {
                    method: String::from("POST"),
                    target: types::HttpTarget::SiteRelative(types::SiteTarget {
                        path: String::from("/attendance.php"),
                        query: Vec::new(),
                        auth_slot: Some(String::from("site-cookie")),
                    }),
                    headers: Vec::new(),
                    body: Some(Vec::new()),
                    max_bytes: 4096,
                })?;
                Ok(types::MaintenanceOutcome {
                    state: types::MaintenanceState::Completed,
                    evidence: format!(
                        "{action_label}:{}:{}:{}",
                        view.name,
                        resp.status,
                        String::from_utf8_lossy(&resp.body)
                    ),
                    session_capture: None,
                })
            }
            "browser-checkin" => {
                let sess = browser::open(browser::Purpose::Maintenance)?;
                let _nav = sess.navigate("/login.php", "load")?;
                let inputs = sess.query("input", 10)?;
                let input_el = inputs
                    .first()
                    .ok_or_else(|| err(types::ErrorCode::NotFound, "missing input"))?;
                sess.fill(
                    &input_el.handle,
                    &browser::FillValue::CredentialSlot(String::from("site-password")),
                )?;
                let _dom = sess.dom(4096)?;
                let _wait = sess.wait_for(None, "networkidle", 1000)?;
                let capture = sess.capture_session()?;
                sess.close()?;
                Ok(types::MaintenanceOutcome {
                    state: types::MaintenanceState::Completed,
                    evidence: format!("{action_label}:browser-ok"),
                    session_capture: Some(capture),
                })
            }
            "browser-wrong-purpose" => {
                let _ = browser::open(browser::Purpose::Search)?;
                Err(err(types::ErrorCode::Conflict, "expected wrong-context"))
            }
            "browser-trap" => {
                let _sess = browser::open(browser::Purpose::Maintenance)?;
                core::arch::wasm32::unreachable();
            }
            "kv-roundtrip" => {
                kv::put("maint-last", action_label.as_bytes(), None)?;
                let got = kv::get("maint-last")?
                    .map(|e| String::from(String::from_utf8_lossy(&e.value).as_ref()))
                    .unwrap_or_default();
                Ok(types::MaintenanceOutcome {
                    state: types::MaintenanceState::AlreadyCompleted,
                    evidence: format!("kv:{got}"),
                    session_capture: None,
                })
            }
            slot_action if slot_action.starts_with("browser-fill-slot:") => {
                let slot = &slot_action["browser-fill-slot:".len()..];
                let sess = browser::open(browser::Purpose::Maintenance)?;
                let _nav = sess.navigate("/login.php", "load")?;
                let inputs = sess.query("input", 10)?;
                let input_el = inputs
                    .first()
                    .ok_or_else(|| err(types::ErrorCode::NotFound, "missing input"))?;
                sess.fill(
                    &input_el.handle,
                    &browser::FillValue::CredentialSlot(String::from(slot)),
                )?;
                sess.close()?;
                Ok(types::MaintenanceOutcome {
                    state: types::MaintenanceState::Completed,
                    evidence: String::from("slot-ok"),
                    session_capture: None,
                })
            }
            other => Ok(types::MaintenanceOutcome {
                state: types::MaintenanceState::Completed,
                evidence: format!("{action_label}:{other}"),
                session_capture: None,
            }),
        }
    }
}

export!(MaintenanceFixture);
