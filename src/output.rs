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
        out.push_str(&self.tags(&l.text));
        out
    }

    /// Colour `#tags` (a letter first, so "#1" and "@handle#12" stay plain), same rule as the web.
    pub fn tags(&self, text: &str) -> String {
        if !self.color {
            return text.to_string();
        }
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
}
