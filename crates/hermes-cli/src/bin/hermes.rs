use std::ffi::OsString;
use std::process::{exit, Command};

fn locate_ultra_binary() -> Option<OsString> {
    let Ok(current) = std::env::current_exe() else {
        return None;
    };
    let Some(dir) = current.parent() else {
        return None;
    };
    let local = dir.join("hermes-agent-ultra");
    if local.exists() {
        Some(local.into_os_string())
    } else {
        None
    }
}

fn main() {
    let mut args = std::env::args_os();
    let _ = args.next();

    let target = locate_ultra_binary().unwrap_or_else(|| OsString::from("hermes-agent-ultra"));
    let status = Command::new(target).args(args).status();

    match status {
        Ok(status) => match status.code() {
            Some(code) => exit(code),
            None => exit(1),
        },
        Err(err) => {
            eprintln!(
                "Failed to launch hermes-agent-ultra. Ensure it is installed and on PATH. ({err})"
            );
            exit(1);
        }
    }
}
