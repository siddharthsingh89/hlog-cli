use crate::api::Line;
use chrono::{Local, TimeZone};
use std::io::IsTerminal;

pub struct Style {
    color: bool,
}

impl Style {
    /// Colors only when stdout is a terminal and NO_COLOR is unset (https://no-color.org).
    /// On Windows the console must also accept colour codes (older consoles print them as `←[2m`).
    pub fn detect() -> Self {
        let no_color = std::env::var_os("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false);
        Style { color: !no_color && std::io::stdout().is_terminal() && enable_ansi() }
    }

    fn paint(&self, code: &str, s: &str) -> String {
        if self.color && !code.is_empty() {
            format!("\x1b[{code}m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }

    pub fn dim(&self, s: &str) -> String {
        self.paint("2", s)
    }

    fn level_code(level: &str) -> &'static str {
        match level {
            "ERROR" => "31",
            "WARN" => "33",
            "LEARN" => "34",
            "WIN" => "32",
            "DEBUG" | "SYSTEM" | "AD" => "2",
            _ => "",
        }
    }

    /// `2026-11-07T09:14:02+05:30 INFO  woke up without alarm. rare.` in local time.
    /// With `who`, prefixes `[handle]` (feed).
    pub fn line(&self, l: &Line, who: bool) -> String {
        let ts = Local
            .timestamp_millis_opt(l.ts_ms)
            .single()
            .map(|d| d.format("%Y-%m-%dT%H:%M:%S%:z").to_string())
            .unwrap_or_else(|| l.ts_ms.to_string());
        let level = format!("{:<5}", l.level);
        let mut out = String::new();
        out.push_str(&self.dim(&ts));
        out.push(' ');
        if who {
            out.push_str(&self.dim(&format!("[{}] ", l.handle)));
        }
        out.push_str(&self.paint(Self::level_code(&l.level), &level));
        out.push(' ');
        out.push_str(&self.tags(&l.text));
        out
    }

    /// Colour `inline code` (a pair of backticks) and `#tags`, same rules as the web: nothing inside
    /// backticks is a tag. The text itself is printed exactly as stored, backticks included.
    pub fn tags(&self, text: &str) -> String {
        if !self.color {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find('`') {
            let Some(len) = rest[open + 1..].find('`').filter(|&n| n > 0) else { break };
            out.push_str(&self.tag_spans(&rest[..open]));
            out.push_str(&self.paint("35", &rest[open..open + len + 2]));
            rest = &rest[open + len + 2..];
        }
        out.push_str(&self.tag_spans(rest));
        out
    }

    fn tag_spans(&self, text: &str) -> String {
        let chars: Vec<char> = text.chars().collect();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < chars.len() {
            let starts_tag = chars[i] == '#'
                && (i == 0 || !(chars[i - 1].is_alphanumeric() || matches!(chars[i - 1], '_' | '@' | '/' | '#')))
                && chars.get(i + 1).is_some_and(|c| c.is_ascii_alphabetic());
            if starts_tag {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '-') && j - i <= 30 {
                    j += 1;
                }
                let tag: String = chars[i..j].iter().collect();
                out.push_str(&self.paint("36", &tag));
                i = j;
            } else {
                out.push(chars[i]);
                i += 1;
            }
        }
        out
    }
}

/// Windows: turn on "virtual terminal processing" for stdout and stderr so colour codes are applied
/// instead of printed. Classic consoles (conhost, older PowerShell windows) have it off by default.
/// Returns false if the console refuses, and then the CLI prints plain text.
#[cfg(windows)]
fn enable_ansi() -> bool {
    type Handle = *mut core::ffi::c_void;
    extern "system" {
        fn GetStdHandle(n: u32) -> Handle;
        fn GetConsoleMode(h: Handle, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: Handle, mode: u32) -> i32;
    }
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    let mut ok = true;
    for which in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain Win32 calls on the process's own standard handles; `mode` is a valid out pointer.
        unsafe {
            let h = GetStdHandle(which);
            let mut mode = 0u32;
            if h.is_null() || GetConsoleMode(h, &mut mode) == 0 {
                // Not a console (redirected): nothing to enable. Only stdout decides whether we colour at all.
                if which == STD_OUTPUT_HANDLE {
                    ok = false;
                }
                continue;
            }
            if mode & ENABLE_VIRTUAL_TERMINAL_PROCESSING == 0 && SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING) == 0 {
                ok = false;
            }
        }
    }
    ok
}

#[cfg(not(windows))]
fn enable_ansi() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::Style;

    #[test]
    fn colours_only_real_tags() {
        let s = Style { color: true };
        let out = s.tags("learned #rust today, see @maya#12 and point #1");
        assert!(out.contains("\x1b[36m#rust\x1b[0m"));
        assert!(!out.contains("\x1b[36m#12"));
        assert!(!out.contains("\x1b[36m#1"));
        assert_eq!(Style { color: false }.tags("plain #rust"), "plain #rust");
    }

    #[test]
    fn colours_inline_code_and_skips_tags_inside_it() {
        let s = Style { color: true };
        let out = s.tags("use `#include <x>` then #cpp, and a lone ` stays");
        assert!(out.contains("\x1b[35m`#include <x>`\x1b[0m"));
        assert!(!out.contains("\x1b[36m#include"));
        assert!(out.contains("\x1b[36m#cpp\x1b[0m"));
        assert!(out.ends_with("a lone ` stays"));
        assert_eq!(s.tags("``"), "``");
    }
}
