use anyhow::Result;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
unsafe extern "C" {
    fn setsid() -> i32;
}

pub(super) fn configure_command_session(command: &mut tokio::process::Command) -> Result<()> {
    #[cfg(unix)]
    {
        unsafe {
            command.pre_exec(|| {
                if setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
    Ok(())
}

pub(super) fn parse_i32_field(value: &str) -> Option<i32> {
    let value = value.trim();
    if value.is_empty()
        || value.eq_ignore_ascii_case("N/A")
        || value.eq_ignore_ascii_case("[Not Supported]")
    {
        None
    } else {
        value.parse().ok()
    }
}

pub(super) fn unix_now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}
