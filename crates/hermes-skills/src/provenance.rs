//! Skill write provenance tracking.
//!
//! Python upstream uses a `ContextVar`; Rust keeps the same nested-scope
//! contract with thread-local state and a guard that restores the prior origin
//! on drop.

use std::cell::RefCell;

pub const FOREGROUND: &str = "foreground";
pub const ASSISTANT_TOOL: &str = "assistant_tool";
pub const BACKGROUND_REVIEW: &str = "background_review";

thread_local! {
    static CURRENT_WRITE_ORIGIN: RefCell<String> = RefCell::new(FOREGROUND.to_string());
}

#[derive(Debug)]
pub struct WriteOriginGuard {
    previous: String,
}

impl Drop for WriteOriginGuard {
    fn drop(&mut self) {
        CURRENT_WRITE_ORIGIN.with(|origin| {
            *origin.borrow_mut() = self.previous.clone();
        });
    }
}

pub fn normalize_write_origin(origin: &str) -> String {
    let trimmed = origin.trim();
    if trimmed.is_empty() {
        FOREGROUND.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn set_current_write_origin(origin: &str) -> WriteOriginGuard {
    CURRENT_WRITE_ORIGIN.with(|current| {
        let previous = current.borrow().clone();
        *current.borrow_mut() = normalize_write_origin(origin);
        WriteOriginGuard { previous }
    })
}

pub fn get_current_write_origin() -> String {
    CURRENT_WRITE_ORIGIN.with(|origin| origin.borrow().clone())
}

pub fn is_background_review() -> bool {
    get_current_write_origin() == BACKGROUND_REVIEW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_origin() {
        let _guard = set_current_write_origin(BACKGROUND_REVIEW);
        assert_eq!(get_current_write_origin(), BACKGROUND_REVIEW);
    }

    #[test]
    fn nested_guard_restores_prior_origin() {
        let outer = set_current_write_origin(ASSISTANT_TOOL);
        {
            let _inner = set_current_write_origin(BACKGROUND_REVIEW);
            assert!(is_background_review());
        }
        assert_eq!(get_current_write_origin(), ASSISTANT_TOOL);
        drop(outer);
        assert_eq!(get_current_write_origin(), FOREGROUND);
    }

    #[test]
    fn empty_origin_falls_back_to_foreground() {
        let _guard = set_current_write_origin("");
        assert_eq!(get_current_write_origin(), FOREGROUND);
    }

    #[test]
    fn origin_is_thread_isolated() {
        let _guard = set_current_write_origin(ASSISTANT_TOOL);
        let inside = std::thread::spawn(|| {
            let _guard = set_current_write_origin(BACKGROUND_REVIEW);
            get_current_write_origin()
        })
        .join()
        .unwrap();
        assert_eq!(inside, BACKGROUND_REVIEW);
        assert_eq!(get_current_write_origin(), ASSISTANT_TOOL);
    }
}
