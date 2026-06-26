#[derive(Debug, Clone)]
pub enum AsrEvent {
    /// `text` = incremental (`new_result`); `full` = SDK cumulative hypothesis (`result`).
    Partial {
        text: String,
        full: Option<String>,
    },
    /// SDK sentence endpoint mid-utterance (`ASR_STATE_FINISH` before flush LAST).
    /// `text` = finalized sentence (`result`); append to committed transcript per ROCKASR2 demo.
    SegmentFinish {
        text: String,
    },
    Final {
        text: String,
    },
    TaskStarted,
    TaskFailed {
        message: String,
    },
}
