#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Listening,
    Thinking,
    Speaking,
}
