//! hlog — a log for humans, from the terminal.
mod api;
mod config;
mod output;

use api::{ApiError, Client, Line, DEFAULT_API};
use output::Style;
use std::io::{BufRead, IsTerminal};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::Duration;

const HELP: &str = "hlog — a log for humans

usage:
  hlog login                       connect this machine (device flow)
  hlog \"shipped the thing\"         append a line (prefix a level: hlog \"WARN disk full\")
  hlog learn \"advisory locks…\"     level shortcuts: info debug warn error learn win
  echo \"from a pipe\" | hlog        one line per input line
  hlog tail [-f] [--level LEARN]   your own log (-f keeps polling)
  hlog feed [-f]                   lines from people you tail
  hlog grep <query>                full-text search your log
  hlog follow @maya [--level LEARN]
  hlog unfollow @maya
  hlog logout                      revoke this machine's token and forget it
  hlog -- tail is a word too       log text that starts with a command name

env: HLOG_API (default https://hlog.dev), NO_COLOR";

const LEVELS: [&str; 6] = ["info", "debug", "warn", "error", "learn", "win"];
const POLL_SECS: u64 = 5;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hlog: {e}");
            ExitCode::FAILURE
        }
    }
}

struct Ctx {
    base: String,
    token: Option<String>,
    style: Style,
}

impl Ctx {
    fn client(&self) -> Client {
        Client::new(&self.base, self.token.clone())
    }
    fn authed(&self) -> Result<Client, String> {
        if self.token.is_none() {
            return Err(format!("not logged in to {} — run `hlog login`", self.base));
        }
        Ok(self.client())
    }
}

fn context() -> Ctx {
    let cfg = config::load();
    let base = std::env::var("HLOG_API").ok().filter(|s| !s.is_empty()).or(cfg.api.clone()).unwrap_or_else(|| DEFAULT_API.to_string());
    let base = base.trim_end_matches('/').to_string();
    // Never send a token to a different server than the one it was issued by.
    let token = if cfg.api.as_deref().map(|a| a.trim_end_matches('/')) == Some(base.as_str()) { cfg.token } else { None };
    Ctx { base, token, style: Style::detect() }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let ctx = context();
    let first = args.first().map(|s| s.as_str());
    match first {
        None => {
            if std::io::stdin().is_terminal() {
                println!("{HELP}");
                Ok(())
            } else {
                append_stdin(&ctx)
            }
        }
        Some("-h") | Some("--help") | Some("help") => {
            println!("{HELP}");
            Ok(())
        }
        Some("-V") | Some("--version") => {
            println!("hlog {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--") => append(&ctx, &args[1..].join(" "), None),
        Some("login") => login(&ctx),
        Some("logout") => logout(&ctx),
        Some("tail") => {
            let (follow_mode, level, rest) = flags(&args[1..])?;
            no_extra(&rest)?;
            tail(&ctx, follow_mode, level)
        }
        Some("feed") => {
            let (follow_mode, level, rest) = flags(&args[1..])?;
            no_extra(&rest)?;
            if level.is_some() {
                return Err("feed has no --level filter (tail with levels using `hlog follow @x --level …`)".into());
            }
            feed(&ctx, follow_mode)
        }
        Some("grep") => {
            let q = args[1..].join(" ");
            if q.trim().is_empty() {
                return Err("usage: hlog grep <query>".into());
            }
            grep(&ctx, &q)
        }
        Some("follow") => {
            let (_, level, rest) = flags(&args[1..])?;
            let handle = one_handle(&rest, "follow")?;
            follow(&ctx, &handle, level)
        }
        Some("unfollow") => {
            let handle = one_handle(&args[1..], "unfollow")?;
            ctx.authed()?.delete(&format!("/follows/{handle}")).map_err(|e| e.to_string())?;
            println!("stopped tailing @{handle}");
            Ok(())
        }
        Some(cmd) if LEVELS.contains(&cmd.to_lowercase().as_str()) && args.len() > 1 => {
            append(&ctx, &args[1..].join(" "), Some(&cmd.to_uppercase()))
        }
        Some(_) => append(&ctx, &args.join(" "), None),
    }
}

/// Parses `-f/--follow` and `--level X` (also `--level=X`, comma lists for follow).
fn flags(args: &[String]) -> Result<(bool, Option<String>, Vec<String>), String> {
    let mut f = false;
    let mut level = None;
    let mut rest = vec![];
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-f" || a == "--follow" {
            f = true;
        } else if a == "--level" || a == "-l" {
            level = Some(it.next().ok_or("--level needs a value")?.to_uppercase());
        } else if let Some(v) = a.strip_prefix("--level=") {
            level = Some(v.to_uppercase());
        } else {
            rest.push(a.clone());
        }
    }
    Ok((f, level, rest))
}

fn no_extra(rest: &[String]) -> Result<(), String> {
    if rest.is_empty() {
        Ok(())
    } else {
        Err(format!("unexpected argument: {}", rest[0]))
    }
}

fn one_handle(rest: &[String], cmd: &str) -> Result<String, String> {
    match rest {
        [h] => Ok(h.trim_start_matches('@').to_string()),
        _ => Err(format!("usage: hlog {cmd} @handle")),
    }
}

fn append(ctx: &Ctx, text: &str, level: Option<&str>) -> Result<(), String> {
    let line = append_with_retry(&ctx.authed()?, text, level)?;
    println!("{}", ctx.style.line(&line, false));
    Ok(())
}

/// Append, waiting out short rate limits (the 2 s burst). Long waits (daily cap) are errors.
fn append_with_retry(client: &Client, text: &str, level: Option<&str>) -> Result<Line, String> {
    for _ in 0..5 {
        match client.append(text, level) {
            Ok(l) => return Ok(l),
            Err(ApiError::Http { status: 429, retry_after: Some(s), .. }) if s <= 60 => sleep(Duration::from_secs(s.max(1))),
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("rate limited — try again later".into())
}

fn append_stdin(ctx: &Ctx) -> Result<(), String> {
    let client = ctx.authed()?;
    let stdin = std::io::stdin();
    let mut first = true;
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        if !first {
            sleep(Duration::from_millis(2100)); // respect the 1 line / 2 s limit
        }
        first = false;
        let l = append_with_retry(&client, &line, None)?;
        println!("{}", ctx.style.line(&l, false));
    }
    Ok(())
}

fn print_oldest_first(ctx: &Ctx, lines: &[Line], who: bool) {
    for l in lines.iter().rev() {
        println!("{}", ctx.style.line(l, who));
    }
}

/// Shared by `tail` and `feed`: print the latest page, then optionally poll for newer lines.
fn stream(ctx: &Ctx, path: &str, base_query: Vec<(&str, String)>, follow_mode: bool, who: bool) -> Result<(), String> {
    let client = ctx.authed()?;
    let mut q = base_query.clone();
    q.push(("limit", "20".into()));
    let page = client.page(path, &q).map_err(|e| e.to_string())?;
    print_oldest_first(ctx, &page.lines, who);
    if !follow_mode {
        return Ok(());
    }
    let mut newest = page.lines.first().map(|l| l.cursor.clone()).unwrap_or_else(|| format!("{}-0", chrono::Utc::now().timestamp_millis()));
    loop {
        sleep(Duration::from_secs(POLL_SECS));
        let mut q = base_query.clone();
        q.push(("after", newest.clone()));
        q.push(("limit", "200".into()));
        match client.page(path, &q) {
            Ok(p) => {
                if let Some(l) = p.lines.first() {
                    newest = l.cursor.clone();
                }
                print_oldest_first(ctx, &p.lines, who);
            }
            Err(ApiError::Network(_)) => continue, // keep tailing through blips
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn tail(ctx: &Ctx, follow_mode: bool, level: Option<String>) -> Result<(), String> {
    let q = level.map(|l| vec![("level", l)]).unwrap_or_default();
    stream(ctx, "/lines", q, follow_mode, false)
}

fn feed(ctx: &Ctx, follow_mode: bool) -> Result<(), String> {
    stream(ctx, "/feed", vec![], follow_mode, true)
}

fn grep(ctx: &Ctx, q: &str) -> Result<(), String> {
    let page = ctx.authed()?.page("/lines", &[("q", q.to_string()), ("limit", "100".into())]).map_err(|e| e.to_string())?;
    if page.lines.is_empty() {
        eprintln!("no matches");
    }
    print_oldest_first(ctx, &page.lines, false);
    Ok(())
}

fn follow(ctx: &Ctx, handle: &str, level: Option<String>) -> Result<(), String> {
    let mut body = serde_json::json!({ "handle": handle });
    if let Some(l) = level {
        body["levels"] = serde_json::json!(l.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect::<Vec<_>>());
    }
    let v = ctx.authed()?.post("/follows", body).map_err(|e| e.to_string())?.unwrap_or_default();
    let levels = v["levels"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_else(|| "all public levels".into());
    println!("now tailing @{} ({})", v["handle"].as_str().unwrap_or(handle), levels);
    Ok(())
}

fn hostname() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok().map(|s| s.trim().to_string()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "cli".into())
        .to_lowercase()
}

fn login(ctx: &Ctx) -> Result<(), String> {
    let client = Client::new(&ctx.base, None);
    let start = client.post("/device/start", serde_json::json!({ "client_name": hostname() })).map_err(|e| e.to_string())?.unwrap_or_default();
    let device = start["device_code"].as_str().ok_or("bad response from server")?.to_string();
    let user_code = start["user_code"].as_str().unwrap_or("?");
    let url = start["verification_url"].as_str().unwrap_or("");
    let mut interval = start["interval"].as_u64().unwrap_or(POLL_SECS);
    println!("open {url} and enter: {user_code}");
    println!("{}", ctx.style.dim("waiting for approval…"));
    loop {
        sleep(Duration::from_secs(interval));
        match client.post("/device/token", serde_json::json!({ "device_code": device })) {
            Ok(Some(v)) => {
                let token = v["token"].as_str().ok_or("bad response from server")?.to_string();
                config::save(&config::Config { api: Some(ctx.base.clone()), token: Some(token) })?;
                println!("logged in. token saved to {}", config::path()?.display());
                return Ok(());
            }
            Ok(None) => return Err("bad response from server".into()),
            Err(ApiError::Http { code, message, .. }) => match code.as_str() {
                "authorization_pending" => continue,
                "slow_down" => interval += 5,
                _ => return Err(message),
            },
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn logout(ctx: &Ctx) -> Result<(), String> {
    if ctx.token.is_some() {
        match ctx.client().post("/logout", serde_json::json!({})) {
            Ok(_) | Err(ApiError::Http { status: 401, .. }) => {}
            Err(e) => return Err(format!("couldn't revoke the token on the server ({e}); nothing was deleted")),
        }
    }
    config::delete()?;
    println!("logged out");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn parses_follow_and_level_flags() {
        assert_eq!(flags(&s(&["-f", "--level", "learn"])).unwrap(), (true, Some("LEARN".into()), vec![]));
        assert_eq!(flags(&s(&["@maya", "--level=win,learn"])).unwrap(), (false, Some("WIN,LEARN".into()), s(&["@maya"])));
        assert!(flags(&s(&["--level"])).is_err());
    }

    #[test]
    fn handles_strip_at() {
        assert_eq!(one_handle(&s(&["@maya"]), "follow").unwrap(), "maya");
        assert!(one_handle(&s(&[]), "follow").is_err());
        assert!(one_handle(&s(&["a", "b"]), "follow").is_err());
    }
}
