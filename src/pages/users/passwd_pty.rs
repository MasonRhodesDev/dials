//! Changing your own password as a standard user. AccountsService's
//! SetPassword always needs administrator rights, so this drives `passwd` on a
//! pseudo-terminal the way a person would: current, new, retype. PAM (and
//! pwquality) stay in charge of what's accepted; their messages become the
//! status line. Runs on the admin worker thread only.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Prompt {
    Current,
    New,
    Retype,
}

/// Classify the tail of passwd's output (LC_ALL=C). Retype is checked before
/// New because "Retype new password:" contains "new password:".
pub fn classify(tail: &str) -> Option<Prompt> {
    let last = tail.trim_end().rsplit('\n').next()?.trim().to_lowercase();
    if !last.ends_with(':') {
        return None;
    }
    if last.contains("retype") || last.contains("again") || last.contains("repeat") {
        Some(Prompt::Retype)
    } else if last.contains("current") || last.contains("old password") {
        Some(Prompt::Current)
    } else if last.contains("new") {
        Some(Prompt::New)
    } else if last.contains("password") {
        // Bare "Password:" from a PAM stack that asks for the old one first.
        Some(Prompt::Current)
    } else {
        None
    }
}

/// The lines PAM printed before a prompt, minus the prompt and passwd's own
/// banner: that's the reason a password was refused.
pub fn reason(transcript: &str) -> String {
    transcript
        .lines()
        .map(str::trim)
        .rfind(|l| {
            !l.is_empty()
                && classify(l).is_none()
                && !l.starts_with("Changing password for")
                && !l.starts_with("passwd: password updated")
                // passwd's sign-off after any failure; the line before says why.
                && *l != "passwd: password unchanged"
        })
        .map(|l| match l.strip_prefix("passwd: ").unwrap_or(l) {
            // The current-password check is the only auth step here.
            "Authentication failure" => "The current password is incorrect.".to_string(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

pub fn change_own(current: &str, new: &str) -> Result<(), String> {
    let (master, slave) = open_pty()?;
    let mut child = unsafe {
        Command::new("passwd")
            .env("LC_ALL", "C")
            .stdin(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?))
            .stdout(Stdio::from(slave.try_clone().map_err(|e| e.to_string())?))
            .stderr(Stdio::from(slave))
            // SAFETY: only async-signal-safe calls between fork and exec.
            // stdin is the slave by now; make it the controlling terminal.
            .pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            })
            .spawn()
            .map_err(|e| format!("passwd: {e}"))?
    };

    let mut master = std::fs::File::from(master);
    let started = Instant::now();
    let mut transcript = String::new();
    let mut pending = String::new();
    let mut new_prompts = 0;
    let mut failure: Option<String> = None;

    loop {
        if started.elapsed() > TIMEOUT {
            failure = Some("passwd didn't finish in time.".into());
            break;
        }
        if !readable(&master, Duration::from_millis(200)) {
            if let Ok(Some(_)) = child.try_wait() {
                break;
            }
            continue;
        }
        let mut buf = [0u8; 512];
        let n = match master.read(&mut buf) {
            // EIO on the master means the child closed the terminal.
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        let chunk = String::from_utf8_lossy(&buf[..n]);
        transcript.push_str(&chunk);
        pending.push_str(&chunk);
        let Some(prompt) = classify(&pending) else {
            continue;
        };
        let answer = match prompt {
            Prompt::Current => current,
            Prompt::New => {
                new_prompts += 1;
                if new_prompts > 1 {
                    // Asked again: the first new password was refused.
                    failure = Some(reason(&pending));
                    break;
                }
                new
            }
            Prompt::Retype => new,
        };
        pending.clear();
        if master
            .write_all(answer.as_bytes())
            .and_then(|()| master.write_all(b"\n"))
            .is_err()
        {
            break;
        }
    }

    if failure.is_some() || started.elapsed() > TIMEOUT {
        let _ = child.kill();
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    // Drain what's left so the reason line is complete.
    let mut rest = String::new();
    let _ = master.read_to_string(&mut rest);
    transcript.push_str(&rest);

    match failure {
        Some(why) if !why.is_empty() => Err(why),
        Some(_) => Err("passwd refused the new password.".into()),
        None if status.success() => Ok(()),
        None => {
            let why = reason(&transcript);
            Err(if why.is_empty() {
                format!("passwd exited with {status}")
            } else {
                why
            })
        }
    }
}

fn open_pty() -> Result<(OwnedFd, OwnedFd), String> {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: openpty writes two fds on success; null name/termios/winsize
    // are allowed. Echo is irrelevant: nothing reads the slave's echo but us.
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(format!("openpty: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: both fds were just returned by openpty and are owned here.
    // Close-on-exec: passwd gets the slave only as its dup'd stdio.
    unsafe {
        libc::fcntl(master, libc::F_SETFD, libc::FD_CLOEXEC);
        libc::fcntl(slave, libc::F_SETFD, libc::FD_CLOEXEC);
        Ok((OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)))
    }
}

fn readable(f: &std::fs::File, wait: Duration) -> bool {
    let mut pfd = libc::pollfd {
        fd: f.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd.
    let rc = unsafe { libc::poll(&mut pfd, 1, wait.as_millis() as libc::c_int) };
    rc > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_from_shadow_utils_and_pam() {
        assert_eq!(
            classify("Changing password for ada.\nCurrent password: "),
            Some(Prompt::Current)
        );
        assert_eq!(classify("(current) UNIX password:"), Some(Prompt::Current));
        assert_eq!(classify("New password: "), Some(Prompt::New));
        assert_eq!(classify("Retype new password: "), Some(Prompt::Retype));
        assert_eq!(
            classify("BAD PASSWORD: it is too short\nRetype new password:"),
            Some(Prompt::Retype)
        );
        assert_eq!(classify("Password: "), Some(Prompt::Current));
        assert_eq!(classify("Changing password for ada."), None);
        assert_eq!(classify(""), None);
    }

    /// Live: runs the real `passwd` for the current user with a wrong
    /// current password, so nothing changes. `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn live_wrong_current_password_is_refused() {
        let err = change_own("definitely-not-my-password", "Unused-New-Pass-123").unwrap_err();
        assert!(!err.is_empty());
        eprintln!("passwd said: {err}");
    }

    #[test]
    fn refusal_reasons() {
        let t = "\nBAD PASSWORD: The password is shorter than 8 characters\nNew password: ";
        assert_eq!(
            reason(t),
            "BAD PASSWORD: The password is shorter than 8 characters"
        );
        let t = "Changing password for ada.\nCurrent password: \npasswd: Authentication token manipulation error\n";
        assert_eq!(reason(t), "Authentication token manipulation error");
        assert_eq!(reason("Changing password for ada.\nNew password:"), "");
        // Recorded from Arch (shadow 4.x, LC_ALL=C), wrong current password.
        let t = "Changing password for mason.\r\nCurrent password: \r\n\
                 passwd: Authentication failure\r\npasswd: password unchanged\r\n";
        assert_eq!(reason(t), "The current password is incorrect.");
    }
}
