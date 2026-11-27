use alloc::{string::String, vec::Vec};

wit_bindgen::generate!({
    path: "../../crates/plugin-api/wit",
    world: "endpoint-plugin",
    pub_export_macro: true,
});

use crawler::plugin::{boost, commands, kv, runtime, types};
use exports::crawler::plugin::{endpoints, lifecycle};

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

struct EndpointFixture;

fn err(code: types::ErrorCode, msg: &str) -> types::ApiError {
    types::ApiError {
        code,
        message: String::from(msg),
        detail: None,
        retry_after_ms: None,
        operation_id: None,
    }
}

impl lifecycle::Guest for EndpointFixture {
    fn validate(_config_schema: String, _state_schema: u32) -> Result<(), types::ApiError> {
        Ok(())
    }

    fn health() -> Result<lifecycle::HealthState, types::ApiError> {
        Ok(lifecycle::HealthState::Healthy)
    }
}

impl endpoints::Guest for EndpointFixture {
    fn handle(
        request: endpoints::EndpointRequest,
    ) -> Result<endpoints::EndpointResponse, types::ApiError> {
        match request.path.as_str() {
            "/submit" => {
                let key = core::str::from_utf8(&request.body)
                    .map_err(|_| err(types::ErrorCode::InvalidArgument, "key not utf8"))?;
                let site = runtime::current_context()
                    .site_id
                    .unwrap_or_else(|| String::from("global"));
                let accepted = commands::submit(&types::CommandRequest {
                    action: types::Command::SiteMaintenance((
                        site,
                        types::MaintenanceAction::VerifySession,
                    )),
                    idempotency_key: String::from(key),
                    expected_revision: None,
                })?;
                Ok(endpoints::EndpointResponse {
                    status: 202,
                    headers: Vec::new(),
                    body: accepted.operation_id.into_bytes(),
                })
            }
            "/boost-snapshot" => {
                let snap = boost::snapshot(&[], &[])?;
                let summary = alloc::format!(
                    "rev={};tasks={};res={}",
                    snap.revision,
                    snap.tasks.len(),
                    snap.reservations.len()
                );
                Ok(endpoints::EndpointResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: summary.into_bytes(),
                })
            }
            "/kv-put" => {
                let etag = kv::put("shared", &request.body, None)?;
                Ok(endpoints::EndpointResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: etag.into_bytes(),
                })
            }
            "/kv-get" => match kv::get("shared")? {
                Some(entry) => Ok(endpoints::EndpointResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: entry.value,
                }),
                None => Ok(endpoints::EndpointResponse {
                    status: 404,
                    headers: Vec::new(),
                    body: Vec::new(),
                }),
            },
            _ => Ok(endpoints::EndpointResponse {
                status: 404,
                headers: Vec::new(),
                body: Vec::new(),
            }),
        }
    }
}

export!(EndpointFixture with_types_in crate::guest);
