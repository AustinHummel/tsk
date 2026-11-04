//! Colour for the human views, in git's spirit: subtle, and only when
//! stdout is a terminal. Decided once at start-up from `--color`, the
//! `NO_COLOR` environment variable, and the store's `color.ui`; `--json`
//! output is never coloured. Plain ANSI escapes, no crate.

use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);

/// `flag` is `--color` (`auto`, `always`, `never`), `config` the store's
/// `color.ui` (`true`, `false`, `always`). Precedence: `--json` off;
/// then the flag; then `NO_COLOR`; then config; then the terminal.
pub fn init(flag: Option<&str>, config: Option<&str>, json: bool) -> Result<(), String> {
    let on = if json {
        false
    } else {
        match flag.unwrap_or("auto") {
            "always" => true,
            "never" => false,
            "auto" => {
                let no_color = std::env::var("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false);
                if no_color {
                    false
                } else {
                    match config.map(|c| c.trim().to_ascii_lowercase()).as_deref() {
                        Some("false") | Some("never") | Some("0") => false,
                        Some("always") => true,
                        _ => is_terminal(),
                    }
                }
            }
            other => return Err(format!("`--color` takes `auto`, `always` or `never`, not `{}`", other)),
        }
    };
    if on {
        enable_vt();
    }
    ON.store(on, Ordering::Relaxed);
    Ok(())
}

pub fn on() -> bool {
    ON.load(Ordering::Relaxed)
}

fn is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

fn paint(code: &str, s: &str) -> String {
    if on() && !s.is_empty() {
        format!("\x1b[{}m{}\x1b[0m", code, s)
    } else {
        s.to_string()
    }
}

pub fn dim(s: &str) -> String {
    paint("2", s)
}
pub fn bold(s: &str) -> String {
    paint("1", s)
}
pub fn green(s: &str) -> String {
    paint("32", s)
}
pub fn red(s: &str) -> String {
    paint("31", s)
}
pub fn yellow(s: &str) -> String {
    paint("33", s)
}
pub fn magenta(s: &str) -> String {
    paint("35", s)
}

/// A status glyph, tinted: todo plain, in progress yellow, paused and
/// dropped dim, complete green, spike magenta.
pub fn glyph(st: crate::state::Status) -> String {
    use crate::state::Status;
    let g = st.glyph();
    match st {
        Status::Todo => g.to_string(),
        Status::InProgress => yellow(g),
        Status::Paused | Status::Dropped => dim(g),
        Status::Complete => green(g),
        Status::Spike => magenta(g),
    }
}

/// `<ready, loop>`: each tag in its colour.
pub fn tags(tags: &[&str]) -> String {
    if tags.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = tags
        .iter()
        .map(|t| match *t {
            "ready" => green(t),
            "loop" => red(t),
            "tentative" => yellow(t),
            other => other.to_string(),
        })
        .collect();
    format!("  <{}>", inner.join(", "))
}

/// Windows consoles interpret escapes only once virtual terminal
/// processing is on; Windows Terminal and Git Bash already have it, the
/// classic console does not. Best effort; a failure leaves the mode alone.
#[cfg(windows)]
fn enable_vt() {
    type Handle = *mut core::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(which: u32) -> Handle;
        fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: Handle, mode: u32) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h.is_null() || h as isize == -1 {
            return;
        }
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) != 0 && mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
fn enable_vt() {}
