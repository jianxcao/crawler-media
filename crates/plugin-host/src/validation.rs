use plugin_api::{Failure, SearchAfter};
use std::collections::HashSet;

pub(crate) fn validate(input: &SearchAfter) -> Result<(), Failure> {
    let mut handles = HashSet::with_capacity(input.candidates.len());
    for candidate in &input.candidates {
        if candidate.handle.is_empty() {
            return Err(Failure::InvalidInput("empty candidate handle".into()));
        }
        if !handles.insert(candidate.handle.as_str()) {
            return Err(Failure::InvalidInput("duplicate candidate handle".into()));
        }
    }
    Ok(())
}
