//! Skill write-origin provenance.
//!
//! Distinguishes agent-sediment skill writes from foreground user-directed
//! writes so the curator can safely manage only autonomously-created skills.
//!
//! Corresponds to `hermes-agent/tools/skill_provenance.py`.

use std::cell::RefCell;

thread_local! {
    static WRITE_ORIGIN: RefCell<String> = const { RefCell::new(String::new()) };
}

/// Sentinel value the background review fork uses.
pub const BACKGROUND_REVIEW: &str = "background_review";

/// Opaque token returned by `set_current_write_origin`. Drop it (or call
/// `reset_current_write_origin`) to restore the previous origin.
pub struct WriteOriginToken {
    previous: String,
}

impl WriteOriginToken {
    /// Restore the prior write origin. Consumes the token.
    pub fn reset(self) {
        let Self { previous } = self;
        WRITE_ORIGIN.with(|cell| {
            *cell.borrow_mut() = previous;
        });
    }
}

/// Set the active write origin for the current thread. Returns a token that
/// must be passed to `reset_current_write_origin` (or dropped via
/// `WriteOriginToken::reset`) to restore the prior value.
///
/// # Example
/// ```ignore
/// let token = set_current_write_origin("background_review");
/// // … tool code runs here …
/// token.reset(); // restore prior origin
/// ```
pub fn set_current_write_origin(origin: &str) -> WriteOriginToken {
    let previous = WRITE_ORIGIN.with(|cell| {
        let prev = cell.borrow().clone();
        *cell.borrow_mut() = origin.to_string();
        prev
    });
    WriteOriginToken { previous }
}

/// Restore the prior write origin using a token.
pub fn reset_current_write_origin(token: WriteOriginToken) {
    token.reset();
}

/// Return the active write origin.
///
/// Default: `"foreground"` — any tool call made by a regular (non-review)
/// agent. `"background_review"` — the self-improvement review fork.
pub fn get_current_write_origin() -> String {
    WRITE_ORIGIN.with(|cell| {
        let val = cell.borrow();
        if val.is_empty() {
            "foreground".to_string()
        } else {
            val.clone()
        }
    })
}

/// Convenience: true iff the current write origin is the background
/// review fork.
pub fn is_background_review() -> bool {
    get_current_write_origin() == BACKGROUND_REVIEW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_is_foreground() {
        assert_eq!(get_current_write_origin(), "foreground");
        assert!(!is_background_review());
    }

    #[test]
    fn test_set_and_restore() {
        let token = set_current_write_origin(BACKGROUND_REVIEW);
        assert_eq!(get_current_write_origin(), BACKGROUND_REVIEW);
        assert!(is_background_review());
        token.reset();
        assert_eq!(get_current_write_origin(), "foreground");
    }

    #[test]
    fn test_nested_origins() {
        let token1 = set_current_write_origin("origin_a");
        assert_eq!(get_current_write_origin(), "origin_a");

        let token2 = set_current_write_origin("origin_b");
        assert_eq!(get_current_write_origin(), "origin_b");

        token2.reset();
        assert_eq!(get_current_write_origin(), "origin_a");

        token1.reset();
        assert_eq!(get_current_write_origin(), "foreground");
    }

    #[test]
    fn empty_origin_defaults_to_foreground() {
        let token = set_current_write_origin("");
        token.reset();
        assert_eq!(get_current_write_origin(), "foreground");
    }
}
