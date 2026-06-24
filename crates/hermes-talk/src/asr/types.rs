#[derive(Debug, Clone)]
pub enum AsrEvent {
    Partial { text: String },
    Final { text: String },
    TaskStarted,
    TaskFailed { message: String },
}
