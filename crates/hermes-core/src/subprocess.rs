//! Cross-platform subprocess helpers.
//!
//! Windows launches console-subsystem children in their own console window when
//! GUI hosts or background workers spawn helpers. Backend helper processes
//! should opt into `CREATE_NO_WINDOW`; terminal UI processes still control
//! stdin/stdout normally through their configured pipes.

pub const WINDOWS_CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn windows_no_window_creation_flags(existing: u32) -> u32 {
    existing | WINDOWS_CREATE_NO_WINDOW
}

pub trait CommandNoWindowExt {
    fn suppress_windows_console(&mut self) -> &mut Self;
}

impl CommandNoWindowExt for std::process::Command {
    fn suppress_windows_console(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(windows_no_window_creation_flags(0));
        }
        self
    }
}

impl CommandNoWindowExt for tokio::process::Command {
    fn suppress_windows_console(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            self.creation_flags(windows_no_window_creation_flags(0));
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_window_flag_preserves_existing_creation_flags() {
        assert_eq!(
            windows_no_window_creation_flags(0x0000_0200),
            WINDOWS_CREATE_NO_WINDOW | 0x0000_0200
        );
    }

    #[test]
    fn std_command_helper_is_chainable_on_all_platforms() {
        let mut command = std::process::Command::new("hermes-noop");
        let returned = command.suppress_windows_console() as *mut _;
        assert_eq!(returned, &mut command as *mut _);
    }

    #[test]
    fn tokio_command_helper_is_chainable_on_all_platforms() {
        let mut command = tokio::process::Command::new("hermes-noop");
        let returned = command.suppress_windows_console() as *mut _;
        assert_eq!(returned, &mut command as *mut _);
    }
}
