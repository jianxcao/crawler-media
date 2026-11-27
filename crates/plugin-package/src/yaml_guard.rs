use crate::PackageError;
use std::marker::PhantomData;
use std::mem::MaybeUninit;

struct ParserGuard<'a> {
    ptr: *mut unsafe_libyaml::yaml_parser_t,
    initialized: bool,
    _input: PhantomData<&'a [u8]>,
}

impl<'a> ParserGuard<'a> {
    fn new(bytes: &'a [u8], path: &str) -> Result<Self, PackageError> {
        // Heap-pin `yaml_parser_t` via raw pointer because `yaml_parser_set_input_string`
        // stores `ptr` inside `(*ptr).read_handler_data` (self-referential pointer).
        let ptr = Box::into_raw(Box::new(
            MaybeUninit::<unsafe_libyaml::yaml_parser_t>::zeroed(),
        ))
        .cast::<unsafe_libyaml::yaml_parser_t>();
        let mut guard = Self {
            ptr,
            initialized: false,
            _input: PhantomData,
        };
        // SAFETY: `guard.ptr` points to an exclusively owned, heap-pinned, zeroed
        // `yaml_parser_t`. Setting `initialized = true` right after initialization succeeds
        // guarantees `Drop` calls `yaml_parser_delete` before freeing the `Box`.
        unsafe {
            if unsafe_libyaml::yaml_parser_initialize(guard.ptr).fail {
                return Err(PackageError::InvalidManifest(format!(
                    "YAML parser initialization failed for `{path}`"
                )));
            }
            guard.initialized = true;
            unsafe_libyaml::yaml_parser_set_encoding(guard.ptr, unsafe_libyaml::YAML_UTF8_ENCODING);
            // SAFETY: `bytes` outlives `guard` for `'a`; `[u8]::as_ptr()` is always non-null
            // (even for empty slices) and `bytes.len()` (`<= MAX_SITE_YAML_BYTES`) fits in `u64`.
            unsafe_libyaml::yaml_parser_set_input_string(
                guard.ptr,
                bytes.as_ptr(),
                bytes.len() as u64,
            );
        }
        Ok(guard)
    }

    fn next_token(&mut self) -> Result<unsafe_libyaml::yaml_token_type_t, ()> {
        let mut token = TokenGuard::new();
        // SAFETY: `self.ptr` is initialized, `bytes` is alive for `'a`, and `token.as_mut_ptr()`
        // points to valid stack-allocated `yaml_token_t` storage.
        let ok = unsafe { unsafe_libyaml::yaml_parser_scan(self.ptr, token.as_mut_ptr()).ok };
        if !ok {
            return Err(());
        }
        Ok(token.token_type())
    }
}

impl Drop for ParserGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: `self.ptr` was allocated via `Box::into_raw` in `ParserGuard::new` and is
        // reclaimed once here on drop; `yaml_parser_delete` frees internal buffers if initialized.
        unsafe {
            if self.initialized {
                unsafe_libyaml::yaml_parser_delete(self.ptr);
            }
            drop(Box::from_raw(
                self.ptr
                    .cast::<MaybeUninit<unsafe_libyaml::yaml_parser_t>>(),
            ));
        }
    }
}

struct TokenGuard {
    raw: MaybeUninit<unsafe_libyaml::yaml_token_t>,
}

impl TokenGuard {
    fn new() -> Self {
        Self {
            raw: MaybeUninit::zeroed(),
        }
    }

    fn as_mut_ptr(&mut self) -> *mut unsafe_libyaml::yaml_token_t {
        self.raw.as_mut_ptr()
    }

    fn token_type(&self) -> unsafe_libyaml::yaml_token_type_t {
        // SAFETY: `raw` is zero-initialized or populated by `yaml_parser_scan`; `type_` is valid.
        unsafe { (*self.raw.as_ptr()).type_ }
    }
}

impl Drop for TokenGuard {
    fn drop(&mut self) {
        // SAFETY: `raw` is either zeroed (`YAML_NO_TOKEN`, no-op in `yaml_token_delete`) or
        // populated by a successful `yaml_parser_scan` call.
        unsafe {
            unsafe_libyaml::yaml_token_delete(self.raw.as_mut_ptr());
        }
    }
}

pub fn reject_yaml_anchors_and_aliases(text: &str, path: &str) -> Result<(), PackageError> {
    let mut parser = ParserGuard::new(text.as_bytes(), path)?;
    loop {
        let ty = parser
            .next_token()
            .map_err(|()| PackageError::InvalidManifest(format!("invalid site YAML `{path}`")))?;
        match ty {
            unsafe_libyaml::YAML_ANCHOR_TOKEN | unsafe_libyaml::YAML_ALIAS_TOKEN => {
                return Err(anchor_err(path));
            }
            unsafe_libyaml::YAML_STREAM_END_TOKEN | unsafe_libyaml::YAML_NO_TOKEN => return Ok(()),
            _ => {}
        }
    }
}

fn anchor_err(path: &str) -> PackageError {
    PackageError::InvalidManifest(format!(
        "YAML anchors and aliases are forbidden in site profile `{path}`"
    ))
}
