#[path = "plugin_http_support/mod.rs"]
mod plugin_http_support;
#[allow(dead_code)]
#[path = "subscribe_hooks_and_admit/support.rs"]
mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use api::{
    ApiDownloadService, ApiState, DownloadOpContext, DownloadOutcome, FrozenDownloadPlan, Store,
    SubmissionPurpose, TorrentSource, router,
};
use axum::http::StatusCode;
use base64::Engine as _;
use domain::{PluginOperationId, Site, SiteId, SubscribeId, Torrent};
use downloader::{DownloadSubmitError, Downloader, MemoryDownloader, torrent_info_hash};
use indexer::{
    AuthType, IndexerError, IndexerProvider, ProviderAvailability, ProviderCallContext,
    ProviderCapability, ProviderDescriptor, ProviderSource, RegistryCandidate, RegistrySnapshot,
    SearchQuery as IndexerSearchQuery,
};
use jobs::Runner as _;
use plugin_api::HOOK_WORLD;
use plugin_http_support::{ADMIN_PASS, HttpEnv, TOKEN, assert_ok, pkg, req_json};
use serde_json::{Value, json};
use store::{BoostTaskState, plugins::PluginStateKind};
use support::{StubSearchFetcher, StubTorrentServer, minimal_torrent_bytes, nexus_row};

const R6_HOOK_WASM_B64: &str = "AGFzbQ0AAQAHqxkBQogBAW0OEXBlcm1pc3Npb24tZGVuaWVkDXdyb25nLWNvbnRleHQJbm90LWZvdW5kEGludmFsaWQtYXJndW1lbnQMc3RhbGUtaGFuZGxlEHN0YWxlLWdlbmVyYXRpb24IY29uZmxpY3QOcXVvdGEtZXhjZWVkZWQEYnVzeRFkZWFkbGluZS1leGNlZWRlZAljYW5jZWxsZWQLdW5zdXBwb3J0ZWQIdXBzdHJlYW0PdW5rbm93bi1vdXRjb21lBAAKZXJyb3ItY29kZQMAAAFtBQxhdXRoLWV4cGlyZWQMcmF0ZS1saW1pdGVkCWNoYWxsZW5nZQduZXR3b3JrBXBhcnNlBAANdXBzdHJlYW0ta2luZAMAAgFxBQh1cHN0cmVhbQEDAA9ibG9ja2VkLWFkZHJlc3MAAA5ib2R5LXRvby1sYXJnZQAAHWJyb3dzZXItaXNvbGF0aW9uLXVuYXZhaWxhYmxlAAARdW5zdXBwb3J0ZWQtcm91dGUAAAQADGVycm9yLWRldGFpbAMABAFrBQFrdwFrcwFyBQRjb2RlAQdtZXNzYWdlcwZkZXRhaWwGDnJldHJ5LWFmdGVyLW1zBwxvcGVyYXRpb24taWQIBAAJYXBpLWVycm9yAwAJAWtzAWtzAWtzAXILDWludm9jYXRpb24taWRzCGNhdXNlLWlkcwlwbHVnaW4taWRzCmdlbmVyYXRpb253D2NvbmZpZy1yZXZpc2lvbncHc3ViamVjdHMHc2l0ZS1pZAsMc3Vic2NyaWJlLWlkDApsaWJyYXJ5LWlkDQtkZWFkbGluZS1tc3cJY2FuY2VsbGVkfwQAB2NvbnRleHQDAA4Ba3cBa3kBa3kBa3cBa38Ba38Ba3MBa3oBcHMBcg0GaGFuZGxlcwdzaXRlLWlkcwV0aXRsZXMEc2l6ZRAHc2VlZGVycxEIbGVlY2hlcnMSDHB1Ymxpc2hlZC1hdBMEZnJlZRQCaHIVB3JlbGVhc2UWBXNjb3JlFwZsYWJlbHMYCHJldmlzaW9udwQADHRvcnJlbnQtdmlldwMAGQFyAgdzaXRlLWlkcwVlcnJvcgoEAAxzaXRlLWZhaWx1cmUDABsBcBoBcBwBcgIKY2FuZGlkYXRlcx0Nc2l0ZS1mYWlsdXJlcx4EAAxzZWFyY2gtYWZ0ZXIDAB8BcHMBcgMHa2V5d29yZHMEcGFnZXkKY2F0ZWdvcmllcyEEAAxzZWFyY2gtcXVlcnkDACIBcHMBcgIFcXVlcnkjCHNpdGUtaWRzJAQADXNlYXJjaC1iZWZvcmUDACUBcBoBcgMHc2l0ZS1pZHMFcXVlcnkjCmNhbmRpZGF0ZXMnBAARc2VhcmNoLXNpdGUtYWZ0ZXIDACgBcBoBcHMBcgIKY2FuZGlkYXRlcyoQYWRtaXR0ZWQtaGFuZGxlcysEAAxmaWx0ZXItYWZ0ZXIDACwBcBoBcgMMc3Vic2NyaWJlLWlkcwhtZWRpYS1pZHMKY2FuZGlkYXRlcy4EAA9zdWJzY3JpYmUtc2NvcmUDAC8BcBoBcHMBcgQMc3Vic2NyaWJlLWlkcwhtZWRpYS1pZHMKY2FuZGlkYXRlczEQYWRtaXR0ZWQtaGFuZGxlczIEABBzdWJzY3JpYmUtc2VsZWN0AwAzAWtzAWtzAXBzAWt3AWt3AXIJDWRvd25sb2FkZXItaWRzEnJlbGF0aXZlLWRpcmVjdG9yeTUIY2F0ZWdvcnk2BHRhZ3M3BnBhdXNlZH8MdXBsb2FkLWxpbWl0OA5kb3dubG9hZC1saW1pdDkKc2VxdWVudGlhbH8QZmlyc3QtbGFzdC1waWVjZX8EABJkb3dubG9hZC1wbGFuLXZpZXcDADoBcgMJY2FuZGlkYXRlGgdwdXJwb3NlcwRwbGFuOwQAD2Rvd25sb2FkLWJlZm9yZQMAPAFyBQhtZWRpYS1pZHMKbGlicmFyeS1pZHMLc291cmNlLXBhdGhzFnByb3Bvc2VkLXJlbGF0aXZlLXBhdGhzCGlzLW1vdmllfwQAD3RyYW5zZmVyLXJlbmFtZQMAPgFxCA1zZWFyY2gtYmVmb3JlASYAEXNlYXJjaC1zaXRlLWFmdGVyASkADHNlYXJjaC1hZnRlcgEgAAxmaWx0ZXItYWZ0ZXIBLQAPc3Vic2NyaWJlLXNjb3JlATAAEHN1YnNjcmliZS1zZWxlY3QBNAAPZG93bmxvYWQtYmVmb3JlAT0AD3RyYW5zZmVyLXJlbmFtZQE/AAQADmZpbHRlci1wYXlsb2FkAwBAAXIDDnNjaGVtYS12ZXJzaW9ueQdjb250ZXh0DwdwYXlsb2FkwQAEAAxmaWx0ZXItaW5wdXQDAEIBcgMGaGFuZGxlcwVkZWx0YXoGcmVhc29ucwQADXJlc2NvcmUtcGF0Y2gDAEQBa3MBcHMBa8cAAXBzAWvJAAFyAwdrZXl3b3JkxgAIc2l0ZS1pZHPIAApjYXRlZ29yaWVzygAEABJzZWFyY2gtcXVlcnktcGF0Y2gDAEsBa3MBa3MBa3MBcHMBcHMBa38Ba3cBa3cBa38Ba38BcgoNZG93bmxvYWRlci1pZM0AEnJlbGF0aXZlLWRpcmVjdG9yec4ACGNhdGVnb3J5zwAIYWRkLXRhZ3PQAAtyZW1vdmUtdGFnc9EABnBhdXNlZNIADHVwbG9hZC1saW1pdNMADmRvd25sb2FkLWxpbWl01AAKc2VxdWVudGlhbNUAEGZpcnN0LWxhc3QtcGllY2XWAAQAE2Rvd25sb2FkLXBsYW4tcGF0Y2gDAFcBcHMBcHMBcQcEZHJvcAFzAAlzZXQtb3JkZXIB2QAAB3Jlc2NvcmUBxQAADHNlYXJjaC1xdWVyeQHMAAAOc2VsZWN0LWhhbmRsZXMB2gAADWRvd25sb2FkLXBsYW4B2AAAC3JlbmFtZS1wYXRoAXMABAAFcGF0Y2gDAFsBcQQEbm9uZQAADXNlYXJjaC1iZWZvcmUBJgAQc3Vic2NyaWJlLXNlbGVjdAE0AA9kb3dubG9hZC1iZWZvcmUBPQAEAAxnYXRlLXBheWxvYWQDAF0BcHMBcgYOc2NoZW1hLXZlcnNpb255B2NvbnRleHQPB2hvb2staWRzB2hhbmRsZXPfAAhyZXZpc2lvbncHcGF5bG9hZN4ABAAKZ2F0ZS1pbnB1dAMAYAFvAndzAXEEBWFsbG93AAAHYWJzdGFpbgAABnJlamVjdAFzAAVkZWZlcgHiAAAEAA1nYXRlLWRlY2lzaW9uAwBjAWtzAWtzAWt3AXIFDG9wZXJhdGlvbi1pZHMNZG93bmxvYWRlci1pZOUACWluZm8taGFzaOYAB291dGNvbWVzFHZlcmlmaWVkLXRvdGFsLWJ5dGVz5wAEABRkb3dubG9hZC1hZnRlci1ldmVudAMAaAFyBQhtZWRpYS1pZHMKbGlicmFyeS1pZHMNcmVsYXRpdmUtcGF0aHMKZmlsZS1jb3VudHkLdG90YWwtYnl0ZXN3BAAYdHJhbnNmZXItY29tcGxldGVkLWV2ZW50AwBqAXIEB3NpdGUtaWRzBmFjdGlvbnMFc3RhdGVzCndpbmRvdy1rZXlzBAAYc2l0ZS1jaGVja2luLWFmdGVyLWV2ZW50AwBsAXEEBG5vbmUAAA5kb3dubG9hZC1hZnRlcgHpAAASdHJhbnNmZXItY29tcGxldGVkAesAABJzaXRlLWNoZWNraW4tYWZ0ZXIB7QAABAANZXZlbnQtcGF5bG9hZAMAbgFwcwFyCA5zY2hlbWEtdmVyc2lvbnkHY29udGV4dA8CaWRzBGtpbmRzC29jY3VycmVkLWF0dwdoYW5kbGVz8AAIcmV2aXNpb253B3BheWxvYWTvAAQABWV2ZW50AwBxAXB9AXIDA2tleXMFdmFsdWXzAARldGFncwQABWVudHJ5AwB0AXICBG5hbWVzBXZhbHVlcwQABmhlYWRlcgMAdgFw9wABa3MBcgMEcGF0aHMFcXVlcnn4AAlhdXRoLXNsb3T5AAQAC3NpdGUtdGFyZ2V0AwB6AXECCnB1YmxpYy11cmwBcwANc2l0ZS1yZWxhdGl2ZQH7AAAEAAtodHRwLXRhcmdldAMAfAFw9wABcH0Ba/8AAXIFBm1ldGhvZHMGdGFyZ2V0/QAHaGVhZGVyc/4ABGJvZHmAAQltYXgtYnl0ZXN3BAAMaHR0cC1yZXF1ZXN0AwCBAQFw9wABcH0Ba3MBcgUGc3RhdHVzewdoZWFkZXJzgwEEYm9keYQBCXRydW5jYXRlZH8HcmVjZWlwdIUBBAANaHR0cC1yZXNwb25zZQMAhgEKHwEAGmNyYXdsZXI6cGx1Z2luL3R5cGVzQDEuMC4wBQAGeQkDAAAJYXBpLWVycm9yAwAADGZpbHRlci1pbnB1dAMAAAVwYXRjaAMAAApnYXRlLWlucHV0AwAADWdhdGUtZGVjaXNpb24DAAAFZXZlbnQDAAAFZW50cnkDAAAMaHR0cC1yZXF1ZXN0AwAADWh0dHAtcmVzcG9uc2UHQgFCCAIDAgEBBAAJYXBpLWVycm9yAwAAAgMCAQcEAAVlbnRyeQMAAgFrAwFqAQQBAQFAAQNrZXlzAAUEAANnZXQBBgocAQAXY3Jhd2xlcjpwbHVnaW4va3ZAMS4wLjAFCgdkAUIJAgMCAQEEAAlhcGktZXJyb3IDAAACAwIBCAQADGh0dHAtcmVxdWVzdAMAAgIDAgEJBAANaHR0cC1yZXNwb25zZQMABAFqAQUBAQFAAQdyZXF1ZXN0AwAGBAAFZmV0Y2gBBwoeAQAZY3Jhd2xlcjpwbHVnaW4vaHR0cEAxLjAuMAULAeMIAGFzbQEAAAABMwZgA39/fwBgEX9/f39/f39/f39/f39/f35/AGAEf39/fwF/YAN/f38Bf2AAAX9gAX8BfwIXAgJrdgNnZXQAAARodHRwBWZldGNoAAEDBwYCAwQFBQUFAwEAAgYIAX8BQYCAAQsHPgcDbWVtAgAHcmVhbGxvYwACCHZhbGlkYXRlAAMGaGVhbHRoAAQGbm90aWZ5AAUGZmlsdGVyAAYEZ2F0ZQAHCroFBiABAX8jACACQQFraiACQQFrQX9zcSEEIAQgA2okACAECxAAQYAgQQBBwAD8CwBBgCALEABBgCBBAEHAAPwLAEGAIAsQAEGAIEEAQcAA/AsAQYAgC7ADAQN/QYAgQQBBgAz8CwAgAEHwAGotAAAhASABQQBGBEBBgAJBA0GAwAAQAEGAwAAtAABFQYjAAC0AAEEBRnEEQEGUwAAoAgAhAkGYwAAoAgAhA0GAJiACNgIAQYQmIAM2AgBBgCRBAzoAAEGgJEEBOgAAQaQkQYAmNgIAQagkQQE2AgBBiCBBgCQ2AgBBjCBBATYCAAtBgCAPCyABQQFGBEBBkAJBC0GAwAAQAEGAwAAtAABFQYjAAC0AAEEBRnEEQCAAQZABaigCAEEBRwRAQYAkQQA6AABBiCRB4AI2AgBBjCRBBjYCAEGIIEGAJDYCAEGMIEEBNgIACwtBgCAPCyABQQVGBEBBgCZB8AI2AgBBhCZBATYCAEGAJEEEOgAAQYgkQYAmNgIAQYwkQQE2AgBBiCBBgCQ2AgBBjCBBATYCAEGAIA8LIAFBBkYEQEGgAkEKQYDAABAAQYDAAC0AAEVBiMAALQAAQQFGcQRAQZTAACgCACECQYAkQQU6AABBvCRBAToAAEG9JCACLQAAQTFGOgAAQYggQYAkNgIAQYwgQQE2AgALQYAgDwtBgCALsQEBAn9BgCBBAEGABPwLAEGwAkEMQYDAABAAQYDAAC0AAEVBiMAALQAAQQFGcQRAQZTAACgCACEBQZjAACgCACECQdACQQNBACABIAJBAEEAQQBBAEEAQQBBAEEAQQBBAEKACEGAxAAQAQtBwAJBCkGAwAAQAEGAwAAtAABFQYjAAC0AAEEBRnEEQEGIIEEDOgAAQZAgQrDqATcDAEGYIEGAAzYCAEGcIEEINgIAC0GAIAsLdwkAQYACCwNjYXQAQZACCwtyZXF1aXJlX2NhdABBoAILCnBhdXNlX21vZGUAQbACCwxjYWxsYmFja191cmwAQcACCwpnYXRlX2RlZmVyAEHQAgsDR0VUAEHgAgsGc2l0ZTowAEHwAgsBMABBgAMLCHI2LWRlZmVyAH0EbmFtZQACAW0BFQIABmt2X2dldAEKaHR0cF9mZXRjaAJSAwIFAANvbGQBB29sZF9sZW4CBWFsaWduAwduZXdfbGVuBANwdHIGBAADYXJnAQRkaXNjAgR2cHRyAwR2bGVuBwMAA2FyZwEEdXB0cgIEdWxlbgcHAQAEYnVtcAGYAQBhc20BAAAAARsCYAN/f38AYBF/f39/f39/f39/f39/f39+fwADAwIAAQQEAXAAAgcdAwN0YmwBAAZrdl9nZXQAAApodHRwX2ZldGNoAAEKOQINACAAIAEgAkEAEQAACykAIAAgASACIAMgBCAFIAYgByAIIAkgCiALIAwgDSAOIA8gEEEBEQEACwAMBG5hbWUABQRzaGltAgQBAAEABgwBAAABAAZrdl9nZXQCCQEBAQNnZXQAAAYQAQAAAQAKaHR0cF9mZXRjaAIaAgEBBWZldGNoAAEAAAICa3YSAQRodHRwEgIGJQQAAgEDA21lbQAAAQMHcmVhbGxvYwEAAQNnZXQBAAIFZmV0Y2gIEwIBAAADAwAEAgABAAEDAwAEAgABagBhc20BAAAAARsCYAN/f38AYBF/f39/f39/f39/f39/f39+fwACFwMBcwN0YmwBcAACAWYBMAAAAWYBMQABCQoBAgBBAAsAAgABAB4EbmFtZQAGBWZpeHVwAQkCAAJmMAECZjEFBAEAAXQGCQEAAQEAA3RibAIeAwEBA3RibAEAAQIBMAADATEABAACAgFzEgQBZhIFBxkBcQIHaGVhbHRoeQAACGRlZ3JhZGVkAXMABhkCAAABAwh2YWxpZGF0ZQAAAQMGaGVhbHRoByYCagABAUACDWNvbmZpZy1zY2hlbWFzDHN0YXRlLXNjaGVtYXkADQgLAQAABQMDAAQCAA4HCgJqAQwBAUAAAA8ICQEAAAYCAwAAEAU2AQEEAAlhcGktZXJyb3IDAQAMaGVhbHRoLXN0YXRlAwwACHZhbGlkYXRlAQIABmhlYWx0aAEDCyQBAB5jcmF3bGVyOnBsdWdpbi9saWZlY3ljbGVAMS4wLjAFAwAGIAMAAAEDBmZpbHRlcgAAAQMEZ2F0ZQAAAQMGbm90aWZ5BxMDcANqAREBAUABBWlucHV0AgASCAsBAAAHAwMABAIAEwcRAmoBBQEBQAEFaW5wdXQEABQICwEAAAgDAwAEAgAVBxACagABAUABBWV2ZW50BgAWCAsBAAAJAwMABAIAFwVtAQEJAAlhcGktZXJyb3IDAQAMZmlsdGVyLWlucHV0AwIABXBhdGNoAwMACmdhdGUtaW5wdXQDBAANZ2F0ZS1kZWNpc2lvbgMFAAVldmVudAMGAAZmaWx0ZXIBBAAEZ2F0ZQEFAAZub3RpZnkBBgsgAQAaY3Jhd2xlcjpwbHVnaW4vaG9va3NAMS4wLjAFBQAA7wIOY29tcG9uZW50LW5hbWUBQwAACAIHcmVhbGxvYwMGbG93X2t2BAhsb3dfaHR0cAUFY192YWwGBmNfaGx0aAcFY19mbHQIBmNfZ2F0ZQkFY19udGYBCAACAQADbWVtARMAEQMAAW0BBHNoaW0CBWZpeHVwAQ0AEgIAAnNoAwRjb3JlAT8BBwAIZl9rdl9nZXQBDGZfaHR0cF9mZXRjaAIFbF92YWwDBmxfaGx0aAQFaF9mbHQFBmhfZ2F0ZQYFaF9udGYBeQMLAAt0eS1pbnN0YW5jZQEJYXBpLWVycm9yAgxmaWx0ZXItaW5wdXQDBXBhdGNoBApnYXRlLWlucHV0BQ1nYXRlLWRlY2lzaW9uBgVldmVudAcFZW50cnkIDGh0dHAtcmVxdWVzdAkNaHR0cC1yZXNwb25zZQwCaHMBLwUFAAV0eXBlcwECa3YCBGh0dHADDmxpZmVjeWNsZV9pbnN0BQpob29rc19pbnN0";

type CallbackAction = Arc<Mutex<Option<Box<dyn FnMut() + Send>>>>;
type RecordedCategoryCalls = Arc<Mutex<Vec<(u32, Vec<String>)>>>;

struct CallbackServer {
    addr: SocketAddr,
    action: CallbackAction,
    stop: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl CallbackServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let action: CallbackAction = Arc::new(Mutex::new(None));
        let (stop_c, act_c) = (Arc::clone(&stop), Arc::clone(&action));
        let handle = std::thread::spawn(move || {
            while !stop_c.load(std::sync::atomic::Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut s, _)) => {
                        let _ = s.set_nonblocking(false);
                        let mut buf = [0u8; 1024];
                        let _ = s.read(&mut buf);
                        if let Some(cb) = act_c.lock().unwrap().as_mut() {
                            cb();
                        }
                        let _ = s.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                        );
                        let _ = s.flush();
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        Self {
            addr,
            action,
            stop,
            handle: Some(handle),
        }
    }

    fn set_action(&self, f: impl FnMut() + Send + 'static) {
        *self.action.lock().unwrap() = Some(Box::new(f));
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/cb", self.addr.port())
    }
}

impl Drop for CallbackServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn make_env(
    dir: &tempfile::TempDir,
    payload: &str,
    dl: Arc<dyn Downloader>,
    addrs: &[SocketAddr],
) -> HttpEnv {
    let store = Arc::new(Store::open(dir.path().join("data")).unwrap());
    let snap = RegistrySnapshot::builtin().unwrap();
    let fetcher = Arc::new(StubSearchFetcher {
        payload: payload.into(),
    });
    let mut state = ApiState::new_arc_with_admin_password(
        store,
        TOKEN.into(),
        Some(ADMIN_PASS.into()),
        snap,
        fetcher,
        dl,
    )
    .unwrap();
    if !addrs.is_empty() {
        let pol = addrs
            .iter()
            .fold(net_guard::TargetPolicy::production(), |p, a| {
                p.with_test_loopback_socket(*a)
            });
        state = state.with_plugin_http_policy_override(pol);
    }
    HttpEnv {
        _dir: tempfile::tempdir_in("/tmp").unwrap(),
        app: router(state.clone()),
        state,
        member_token: String::new(),
    }
}

fn seed_site(env: &HttpEnv, name: &str, url: &str, profile: &str) -> SiteId {
    let id = SiteId::new();
    env.state
        .store()
        .insert_site(&Site {
            id,
            name: name.into(),
            url: url.into(),
            profile_id: profile.into(),
            cookie: Some("uid=1; pass=init".into()),
            api_key: None,
            rss_url: None,
            proxy: None,
            rate_limit_per_minute: None,
            cdp_url: None,
            downloader_id: None,
            enabled: true,
            maintenance_via: domain::MaintenanceVia::Http,
            browser_search: false,
            timezone: "UTC".into(),
        })
        .unwrap();
    id
}

async fn post_json(env: &HttpEnv, uri: &str, body: Value) -> (StatusCode, Value) {
    let (st, _, b) = env
        .send(req_json("POST", uri, Some(TOKEN), &[], Some(body)))
        .await;
    (st, b)
}

async fn create_tv_sub(env: &HttpEnv, title: &str) -> SubscribeId {
    let cov = json!({ "kind": "tv", "season": 1, "episode_from": 1, "episode_to": 2 });
    let media = json!({ "kind": "tv", "title": title, "original_title": title });
    let body = json!({
        "media": media,
        "coverage": cov,
        "fetch_mode": "search",
        "follow_future": false
    });
    let (st, b) = post_json(env, "/api/v1/subscriptions", body).await;
    assert_ok(st, &b, StatusCode::CREATED)["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn install_r6_hook_plugin(
    env: &HttpEnv,
    id: &str,
    hooks: &[&str],
    cb_addr: Option<SocketAddr>,
) -> store::plugins::PluginStateAuthority {
    env.enable_dev_mode().await;
    let wasm = base64::engine::general_purpose::STANDARD
        .decode(R6_HOOK_WASM_B64)
        .unwrap();
    let hooks_toml: String = hooks
        .iter()
        .map(|h| format!("[[components.hooks]]\nid = \"{h}\"\npriority = 10\n"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut caps: Vec<String> = hooks.iter().map(|h| format!("hooks:{h}")).collect();
    caps.push("kv".into());
    let http_toml = if let Some(addr) = cb_addr {
        caps.push("http".into());
        format!(
            "\n[[http]]\norigin = \"http://{addr}\"\npath_prefix = \"/cb\"\nmethods = [\"GET\"]\n"
        )
    } else {
        String::new()
    };
    let perms_str = caps
        .iter()
        .map(|c| format!("\"{c}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let schema = pkg::sample_schema(false, None);
    let manifest = format!(
        "manifest_version = 1\nid = \"{id}\"\nname = \"R6 {id}\"\nversion = \"1.0.0\"\napi = \"1.0.0\"\nlicense = \"MIT\"\nauthors = [\"test\"]\ndescription = \"R6\"\nconfig_schema = \"config.schema.json\"\nstate_schema = 1\n{http_toml}\n[[components]]\nid = \"policy\"\nentry = \"policy.wasm\"\nworld = \"{HOOK_WORLD}\"\npermissions = [{perms_str}]\n\n{hooks_toml}\n"
    );
    let zip = pkg::build_stored_zip(&[
        ("plugin.toml", manifest.as_bytes()),
        ("config.schema.json", schema.as_bytes()),
        ("policy.wasm", &wasm),
    ]);
    let grants: Vec<_> = caps
        .iter()
        .map(|c| json!({ "capability": c, "scope": "Global", "sensitive": false }))
        .collect();
    let inst = env.install_pkg(&zip, None, json!(grants)).await;
    env.enable_plugin(id, inst["revision"].as_i64().unwrap(), &format!("en-{id}"))
        .await;
    let plugin = env.state.store().get_plugin(id).unwrap().unwrap();
    env.state
        .store()
        .issue_plugin_state_authority(&plugin, None)
        .unwrap()
}

fn put_kv(env: &HttpEnv, auth: &store::plugins::PluginStateAuthority, key: &str, val: &str) {
    let rev = env
        .state
        .store()
        .plugin_state_get(auth, PluginStateKind::Kv, key)
        .unwrap()
        .map_or(0, |e| e.revision);
    env.state
        .store()
        .plugin_state_put(auth, PluginStateKind::Kv, key, val.as_bytes(), rev)
        .unwrap();
}

async fn enqueue_and_run_search(env: &HttpEnv, sub_id: SubscribeId) -> Result<(), String> {
    let job = {
        let jobs = env.state.jobs();
        let q = jobs.acquire().unwrap();
        let payload = json!({ "subscribe_id": sub_id.to_string() }).to_string();
        q.enqueue(jobs::JobIdentity::subscribe_search(sub_id), &payload, 0)
            .unwrap();
        q.claim(store::unix_now() + 3600, 10)
            .unwrap()
            .into_iter()
            .find(|j| j.scope == jobs::JobScope::Subscribe(sub_id))
            .unwrap()
    };
    let st = env.state.clone();
    tokio::task::spawn_blocking(move || api::worker::StateRunner { state: st }.run(&job))
        .await
        .unwrap()
}

fn build_plan_and_ctx(
    (site_id, url, title): (SiteId, &str, &str),
    purpose: SubmissionPurpose,
    plugin_id: Option<&str>,
    idem_key: &str,
) -> (FrozenDownloadPlan, DownloadOpContext) {
    let torrent = Torrent {
        site_id,
        id: Some("1".into()),
        title: title.into(),
        enclosure: url.into(),
        size_bytes: Some(4096),
        seeders: Some(10),
        leechers: None,
        snatched: None,
        upload_time: None,
        poster_url: None,
        free: true,
        hr: Some(false),
        imdb_id: None,
        detail_url: None,
        category: None,
        free_2x: false,
        free_until: None,
    };
    let plan = FrozenDownloadPlan {
        torrent,
        source: TorrentSource::from_url(url).unwrap(),
        purpose,
        downloader_id: None,
        frozen_endpoint: None,
        subscribe_id: None,
        subscribe_generation: None,
        policy_revision: None,
        media_id: None,
        score: Some(80),
        release_override: None,
        force: false,
        save_path: None,
        category: None,
        tags: vec![],
        paused: false,
        upload_limit: None,
        download_limit: None,
        sequential: false,
        first_last_piece: false,
        budget_key: None,
        plan_digest: None,
    };
    let op_id = PluginOperationId::new();
    let ctx = DownloadOpContext {
        operation_id: op_id,
        cause_id: format!("r6:{op_id}"),
        subject: "user:r6".into(),
        idempotency_key: idem_key.into(),
        plugin_id: plugin_id.map(str::to_string),
        generation: None,
        config_revision: None,
        dry_run: false,
    };
    (plan, ctx)
}

#[tokio::test]
async fn subscribe_select_defer_does_not_poison_bumped_policy_revision_or_stale_subscribe() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start(minimal_torrent_bytes("RaceShow.S01E01.1080p.WEB-DL", 4096));
    let cb = CallbackServer::start();
    let enc = srv.url("race-e01.torrent");
    let html = nexus_row("RaceShow.S01E01.1080p.WEB-DL", &enc);
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, &html, dl, &[srv.addr, cb.addr]);
    env.state
        .store()
        .put_setting(api::settings_keys::SUBSCRIBE_AIR_DATE_GATE, "false")
        .unwrap();
    seed_site(&env, "RaceSite", &srv.url(""), "demo");
    let auth = install_r6_hook_plugin(
        &env,
        "org.example.r6-select",
        &["subscribe.select"],
        Some(cb.addr),
    )
    .await;
    put_kv(&env, &auth, "callback_url", &cb.url());
    put_kv(&env, &auth, "gate_defer", "1");

    // 1) Worker finish_search: policy_revision bumps R -> R+1 mid-select (ensure_current still passes).
    let sub_w = create_tv_sub(&env, "RaceShow").await;
    let st_w = env.state.clone();
    cb.set_action(move || {
        let current = st_w.store().get_subscribe(sub_w).unwrap().unwrap();
        st_w.store().update_subscribe(&current).unwrap();
    });
    enqueue_and_run_search(&env, sub_w).await.unwrap();
    assert_eq!(
        env.state
            .store()
            .subscribe_policy_binding(sub_w)
            .unwrap()
            .unwrap()
            .policy_revision,
        2
    );
    assert!(
        env.state
            .store()
            .list_subscribe_defers(sub_w, 10)
            .unwrap()
            .is_empty(),
        "stale R select defer must not be recorded under R+1 in finish_search"
    );

    // 2) Manual run_subscribe: policy_revision bumps R -> R+1 mid-select.
    let sub_m = create_tv_sub(&env, "RaceShow").await;
    let st_m = env.state.clone();
    cb.set_action(move || {
        let current = st_m.store().get_subscribe(sub_m).unwrap().unwrap();
        st_m.store().update_subscribe(&current).unwrap();
    });
    let (st, body) = post_json(
        &env,
        &format!("/api/v1/subscriptions/{sub_m}/run"),
        json!({}),
    )
    .await;
    assert_ok(st, &body, StatusCode::OK);
    assert!(
        env.state
            .store()
            .list_subscribe_defers(sub_m, 10)
            .unwrap()
            .is_empty(),
        "stale R select defer must not be recorded under R+1 in manual run"
    );

    // 3) Subscribe paused mid-select fails ensure_current BEFORE record_select_defers runs.
    let sub_p = create_tv_sub(&env, "RaceShow").await;
    let st_p = env.state.clone();
    cb.set_action(move || {
        let mut current = st_p.store().get_subscribe(sub_p).unwrap().unwrap();
        current.tracking_state = "paused".into();
        st_p.store().update_subscribe(&current).unwrap();
    });
    let _ = enqueue_and_run_search(&env, sub_p).await;
    assert!(
        env.state
            .store()
            .list_subscribe_defers(sub_p, 10)
            .unwrap()
            .is_empty(),
        "paused subscribe must fail ensure_current before recording select defers"
    );
}

#[tokio::test]
async fn download_before_defer_uses_pre_hook_binding_and_rechecks_submission_fence() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start(minimal_torrent_bytes("DlRace.S01E01.1080p.WEB-DL", 4096));
    let cb = CallbackServer::start();
    let enc = srv.url("dl-race.torrent");
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, "", dl, &[srv.addr, cb.addr]);
    let site_id = seed_site(&env, "DlRaceSite", &srv.url(""), "demo");
    let auth = install_r6_hook_plugin(
        &env,
        "org.example.r6-dl",
        &["download.before"],
        Some(cb.addr),
    )
    .await;
    put_kv(&env, &auth, "callback_url", &cb.url());
    put_kv(&env, &auth, "gate_defer", "1");
    let svc = ApiDownloadService::new(env.state.clone());

    // 1) policy_revision bumps R -> R+1 during run_download_before -> StaleAuthority, R+1 stays clean.
    let sub1 = create_tv_sub(&env, "DlRace").await;
    let st1 = env.state.clone();
    cb.set_action(move || {
        let current = st1.store().get_subscribe(sub1).unwrap().unwrap();
        st1.store().update_subscribe(&current).unwrap();
    });
    let (mut p1, c1) = build_plan_and_ctx(
        (site_id, &enc, "DlRace.S01E01.1080p.WEB-DL"),
        SubmissionPurpose::Subscribe,
        None,
        "dl-r6-1",
    );
    p1.subscribe_id = Some(sub1);
    let out1 = svc.submit_sync(p1, c1).unwrap();
    assert!(matches!(out1, DownloadOutcome::Deferred { .. }));
    assert!(
        env.state
            .store()
            .list_subscribe_defers(sub1, 10)
            .unwrap()
            .is_empty(),
        "download.before defer must use pre-hook binding and not poison R+1"
    );

    // 2) Explicit stale subscribe_generation + policy_revision on FrozenDownloadPlan is honored.
    cb.set_action(|| {});
    let sub2 = create_tv_sub(&env, "DlRace2").await;
    let (mut p2, c2) = build_plan_and_ctx(
        (site_id, &enc, "DlRace2.S01E01.1080p.WEB-DL"),
        SubmissionPurpose::Subscribe,
        None,
        "dl-r6-2",
    );
    p2.subscribe_id = Some(sub2);
    p2.subscribe_generation = Some(999_999);
    p2.policy_revision = Some(1);
    let out2 = svc.submit_sync(p2, c2).unwrap();
    assert!(matches!(out2, DownloadOutcome::Deferred { .. }));
    assert!(
        env.state
            .store()
            .list_subscribe_defers(sub2, 10)
            .unwrap()
            .is_empty()
    );

    // 3) Subscribe deleted during run_download_before fails re-checked check_submission_fence on Gate::Defer.
    let sub3 = create_tv_sub(&env, "DlRace3").await;
    let st3 = env.state.clone();
    cb.set_action(move || {
        st3.store().delete_subscribe(sub3).unwrap();
    });
    let (mut p3, c3) = build_plan_and_ctx(
        (site_id, &enc, "DlRace3.S01E01.1080p.WEB-DL"),
        SubmissionPurpose::Subscribe,
        None,
        "dl-r6-3",
    );
    p3.subscribe_id = Some(sub3);
    let err3 = svc.submit_sync(p3, c3).unwrap_err();
    assert!(
        matches!(err3, DownloadSubmitError::Cancelled),
        "expected Cancelled when subscribe deleted during download.before defer, got {err3:?}"
    );
}

struct CategoryCaptureProvider {
    desc: ProviderDescriptor,
    enc_match: String,
    enc_filler: String,
    calls: RecordedCategoryCalls,
}

impl IndexerProvider for CategoryCaptureProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.desc
    }
    fn search(
        &self,
        _: &ProviderCallContext<'_>,
        site: &Site,
        query: &IndexerSearchQuery<'_>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        self.calls
            .lock()
            .unwrap()
            .push((query.page, query.categories.to_vec()));
        if query.page == 1 && query.keyword.contains("S01") {
            return Ok((0..30)
                .map(|i| Torrent {
                    site_id: site.id,
                    id: Some(format!("irr-{i}")),
                    title: format!("OtherShow.S02E{i:02}.1080p.WEB-DL"),
                    enclosure: format!("{}?irr={i}", self.enc_filler),
                    size_bytes: Some(4096),
                    seeders: Some(5),
                    leechers: None,
                    snatched: None,
                    upload_time: None,
                    poster_url: None,
                    free: true,
                    hr: Some(false),
                    imdb_id: None,
                    detail_url: None,
                    category: None,
                    free_2x: false,
                    free_until: None,
                })
                .collect());
        }
        if query.page == 2 {
            return Ok(vec![Torrent {
                site_id: site.id,
                id: Some("hit-1".into()),
                title: "CatShow.S01E01.1080p.WEB-DL".into(),
                enclosure: self.enc_match.clone(),
                size_bytes: Some(4096),
                seeders: Some(20),
                leechers: None,
                snatched: None,
                upload_time: None,
                poster_url: None,
                free: true,
                hr: Some(false),
                imdb_id: None,
                detail_url: None,
                category: None,
                free_2x: false,
                free_until: None,
            }]);
        }
        Ok(vec![])
    }
    fn rss(
        &self,
        _: &ProviderCallContext<'_>,
        _: &Site,
        _: Option<&str>,
    ) -> Result<Vec<Torrent>, IndexerError> {
        Ok(vec![])
    }
    fn resolve_download(
        &self,
        _: &ProviderCallContext<'_>,
        _: &Site,
        _: &Torrent,
    ) -> Result<Option<indexer::ResolvedDownload>, IndexerError> {
        Ok(None)
    }
}

#[tokio::test]
async fn subscribe_search_passes_search_before_categories_to_indexer_and_site_after_across_pages() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let srv = StubTorrentServer::start(minimal_torrent_bytes("CatShow.S01E01.1080p.WEB-DL", 4096));
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, "", dl.clone(), &[srv.addr]);
    env.state
        .store()
        .put_setting(api::settings_keys::SUBSCRIBE_AIR_DATE_GATE, "false")
        .unwrap();

    let calls = Arc::new(Mutex::new(Vec::new()));
    let provider = CategoryCaptureProvider {
        desc: ProviderDescriptor {
            id: "r6-cat-prov".into(),
            name: "R6CatProv".into(),
            kind: "wasm".into(),
            source: ProviderSource::Builtin,
            auth_types: vec![AuthType::Cookie],
            capabilities: vec![ProviderCapability::Search],
            availability: ProviderAvailability::Enabled,
            hidden_from_catalog: false,
        },
        enc_match: srv.url("cat-e01.torrent"),
        enc_filler: srv.url("filler.torrent"),
        calls: Arc::clone(&calls),
    };
    let auth = install_r6_hook_plugin(
        &env,
        "org.example.r6-search-cats",
        &["search.before", "search.site.after"],
        None,
    )
    .await;
    put_kv(&env, &auth, "cat", "cat-tv-hd");
    put_kv(&env, &auth, "require_cat", "1");

    let cand = RegistryCandidate::from_builtin()
        .unwrap()
        .add_indexer_provider(Arc::new(provider))
        .unwrap();
    env.state.indexer().publish(cand).unwrap();
    seed_site(&env, "CatSite", &srv.url(""), "r6-cat-prov");

    let sub_id = create_tv_sub(&env, "CatShow").await;
    enqueue_and_run_search(&env, sub_id).await.unwrap();

    let recorded = calls.lock().unwrap().clone();
    assert!(
        recorded
            .iter()
            .any(|(p, c)| *p == 1 && c == &vec!["cat-tv-hd".to_string()]),
        "page 1 must receive patched categories: {recorded:?}"
    );
    assert!(
        recorded
            .iter()
            .any(|(p, c)| *p == 2 && c == &vec!["cat-tv-hd".to_string()]),
        "page 2 must receive patched categories: {recorded:?}"
    );
    assert_eq!(
        dl.submitted().len(),
        1,
        "search.site.after must receive patched categories so page-2 candidate survives and submits"
    );
}

#[tokio::test]
async fn boost_task_fact_records_final_view_paused_patched_by_download_before() {
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let b1 = minimal_torrent_bytes("BoostPause.One.2026.1080p", 4096);
    let b2 = minimal_torrent_bytes("BoostPause.Two.2026.1080p", 4096);
    let (h1, h2) = (
        torrent_info_hash(&b1).unwrap(),
        torrent_info_hash(&b2).unwrap(),
    );
    let (srv1, srv2) = (StubTorrentServer::start(b1), StubTorrentServer::start(b2));
    let dl = Arc::new(MemoryDownloader::new(dir.path().join("stage")));
    let env = make_env(&dir, "", dl.clone(), &[srv1.addr, srv2.addr]);
    let site_id = seed_site(&env, "BoostPauseSite", &srv1.url(""), "demo");
    let auth =
        install_r6_hook_plugin(&env, "org.example.r6-pause", &["download.before"], None).await;
    let svc = ApiDownloadService::new(env.state.clone());

    // 1) plan.paused == false, hook patches paused = Some(true) -> BoostTaskState::Paused
    put_kv(&env, &auth, "pause_mode", "1");
    let (mut p1, c1) = build_plan_and_ctx(
        (
            site_id,
            &srv1.url("bp1.torrent"),
            "BoostPause.One.2026.1080p",
        ),
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "bp-r6-1",
    );
    p1.paused = false;
    assert!(matches!(
        svc.submit_sync(p1, c1).unwrap(),
        DownloadOutcome::Submitted { .. }
    ));
    assert!(dl.is_task_paused(&h1));
    let facts1 = env.state.store().list_active_boost_task_facts().unwrap();
    let f1 = facts1.iter().find(|f| f.info_hash == h1).unwrap();
    assert_eq!(
        f1.state,
        BoostTaskState::Paused,
        "boost_task_facts must reflect final_view.paused == true"
    );

    // 2) plan.paused == true, hook patches paused = Some(false) -> BoostTaskState::Downloading
    put_kv(&env, &auth, "pause_mode", "0");
    let (mut p2, c2) = build_plan_and_ctx(
        (
            site_id,
            &srv2.url("bp2.torrent"),
            "BoostPause.Two.2026.1080p",
        ),
        SubmissionPurpose::Boost,
        Some("cm.boost"),
        "bp-r6-2",
    );
    p2.paused = true;
    assert!(matches!(
        svc.submit_sync(p2, c2).unwrap(),
        DownloadOutcome::Submitted { .. }
    ));
    assert!(!dl.is_task_paused(&h2));
    let facts2 = env.state.store().list_active_boost_task_facts().unwrap();
    let f2 = facts2.iter().find(|f| f.info_hash == h2).unwrap();
    assert_eq!(
        f2.state,
        BoostTaskState::Downloading,
        "boost_task_facts must reflect final_view.paused == false"
    );
}
