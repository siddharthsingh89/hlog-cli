use crate::api::Line;
use chrono::{Local, TimeZone};
use std::io::IsTerminal;

pub struct Style {
    color: bool,
}

impl Style {
    /// Colors only when stdout is a terminal and NO_COLOR is unset (https://no-color.org).
    pub fn detect() -> Self {
        let no_color = std::env::var_os("NO_COLOR").map(|v| !v.is_empty()).unwrap_or(false);
        Style { color: !no_color && std::io::stdout().is_terminal() }
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
        out.push_str(&l.text);
        out
    }
}
