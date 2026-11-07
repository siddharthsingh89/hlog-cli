//! hlog — a log for humans, from the terminal.
mod api;
mod config;
mod output;
mod queue;

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
  hlog sync                        send lines that were queued while offline
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
        Some("sync") => sync(&ctx),
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

/// Errors where the line itself is fine and sending later can succeed: keep it in the queue.
/// (Validation errors like "line is longer than 500 characters" are not retryable.)
fn retryable(e: &ApiError) -> bool {
    match e {
        ApiError::Network(_) => true,
        ApiError::Http { status, .. } => *status == 401 || *status == 429 || *status >= 500,
    }
}

/// Append, waiting out short rate limits (the 2 s burst). Long waits (daily cap) are returned as errors.
fn append_with_retry(client: &Client, text: &str, level: Option<&str>) -> Result<Line, ApiError> {
    let mut last = None;
    for _ in 0..5 {
        match client.append(text, level) {
            Ok(l) => return Ok(l),
            Err(ApiError::Http { status: 429, retry_after: Some(s), code, message }) if s <= 60 => {
                sleep(Duration::from_secs(s.max(1)));
                last = Some(ApiError::Http { status: 429, retry_after: Some(s), code, message });
            }
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| ApiError::Network("rate limited".into())))
}

fn enqueue(ctx: &Ctx, text: &str, level: Option<&str>, why: &str) -> Result<(), String> {
    queue::push(&queue::Queued { api: ctx.base.clone(), text: text.to_string(), level: level.map(str::to_string), queued_at: queue::now_local() })?;
    eprintln!("{} {why}. saved locally; it'll be sent by `hlog sync` or with your next line.", ctx.style.dim("queued:"));
    Ok(())
}

fn why(e: &ApiError) -> String {
    match e {
        ApiError::Network(_) => "can't reach hlog (offline?)".into(),
        ApiError::Http { status: 401, .. } => "not logged in (run `hlog login`)".into(),
        ApiError::Http { status: 429, message, .. } => message.clone(),
        ApiError::Http { status, .. } => format!("hlog had a problem (HTTP {status})"),
    }
}

/// Send queued lines for this server, oldest first, paced at the 1 line / 2 s limit. Stops at the first
/// retryable failure (the rest stay queued). Returns (sent, still queued).
fn flush(ctx: &Ctx, client: &Client, verbose: bool) -> Result<(usize, usize), String> {
    let all = queue::load();
    let (mine, others): (Vec<_>, Vec<_>) = all.into_iter().partition(|q| q.api == ctx.base);
    if mine.is_empty() {
        return Ok((0, 0));
    }
    let mut sent = 0;
    let mut keep = Vec::new();
    let mut stopped = false;
    for (i, q) in mine.iter().enumerate() {
        if stopped {
            keep.push(q.clone());
            continue;
        }
        if i > 0 {
            sleep(Duration::from_millis(2100));
        }
        match append_with_retry(client, &q.text, q.level.as_deref()) {
            Ok(l) => {
                sent += 1;
                println!("{} {}", ctx.style.line(&l, false), ctx.style.dim(&format!("(queued {})", q.queued_at)));
            }
            Err(e) if retryable(&e) => {
                if verbose {
                    eprintln!("stopped: {}", why(&e));
                }
                keep.push(q.clone());
                stopped = true;
            }
            Err(e) => eprintln!("dropped a queued line that can't be sent ({e}): {}", q.text),
        }
    }
    let left = keep.len();
    let mut rest = others;
    rest.extend(keep);
    queue::save(&rest)?;
    Ok((sent, left))
}

fn append(ctx: &Ctx, text: &str, level: Option<&str>) -> Result<(), String> {
    if ctx.token.is_none() {
        return enqueue(ctx, text, level, "not logged in (run `hlog login`, then `hlog sync`)");
    }
    let client = ctx.client();
    // Older queued lines go first, so the log keeps the order things were typed in.
    let (sent, left) = flush(ctx, &client, false)?;
    if left > 0 {
        return enqueue(ctx, text, level, &format!("{left} earlier line(s) are still waiting to send"));
    }
    if sent > 0 {
        sleep(Duration::from_millis(2100)); // pace after the queued lines
    }
    match append_with_retry(&client, text, level) {
        Ok(line) => {
            println!("{}", ctx.style.line(&line, false));
            Ok(())
        }
        Err(e) if retryable(&e) => enqueue(ctx, text, level, &why(&e)),
        Err(e) => Err(e.to_string()),
    }
}

fn append_stdin(ctx: &Ctx) -> Result<(), String> {
    let stdin = std::io::stdin();
    let client = ctx.client();
    // Once one line fails for a retryable reason, queue the rest without trying (keeps order).
    let mut failing: Option<String> = if ctx.token.is_none() { Some("not logged in".into()) } else { None };
    if failing.is_none() {
        let (_, left) = flush(ctx, &client, false)?;
        if left > 0 {
            failing = Some("earlier queued lines are still waiting".into());
        }
    }
    let mut first = true;
    let mut queued = 0;
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        if failing.is_some() {
            queue::push(&queue::Queued { api: ctx.base.clone(), text: line, level: None, queued_at: queue::now_local() })?;
            queued += 1;
            continue;
        }
        if !first {
            sleep(Duration::from_millis(2100)); // respect the 1 line / 2 s limit
        }
        first = false;
        match append_with_retry(&client, &line, None) {
            Ok(l) => println!("{}", ctx.style.line(&l, false)),
            Err(e) if retryable(&e) => {
                failing = Some(why(&e));
                queue::push(&queue::Queued { api: ctx.base.clone(), text: line, level: None, queued_at: queue::now_local() })?;
                queued += 1;
            }
            Err(e) => eprintln!("skipped ({e}): {line}"),
        }
    }
    if queued > 0 {
        eprintln!(
            "{} {queued} line(s): {}. saved locally; run `hlog sync` later.",
            ctx.style.dim("queued:"),
            failing.unwrap_or_default()
        );
    }
    Ok(())
}

fn sync(ctx: &Ctx) -> Result<(), String> {
    let pending = queue::load().into_iter().filter(|q| q.api == ctx.base).count();
    if pending == 0 {
        println!("nothing queued");
        return Ok(());
    }
    let client = ctx.authed()?;
    let (sent, left) = flush(ctx, &client, true)?;
    println!("sent {sent} queued line(s){}", if left > 0 { format!(", {left} still queued") } else { String::new() });
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
                println!();
                println!("next:");
                println!("  hlog learn \"something you figured out today\"   # write your first line (private by default)");
                println!("  hlog tail                                         # read it back");
                println!("  revoke this machine any time: hlog logout, or hlog.dev/settings/tokens");
                let pending = queue::load().into_iter().filter(|q| q.api == ctx.base).count();
                if pending > 0 {
                    println!("you have {pending} queued line(s) — run `hlog sync` to send them");
                }
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
    let pending = queue::load().len();
    if pending > 0 {
        println!("note: {pending} queued line(s) are kept locally; they'll send after `hlog login` + `hlog sync`");
    }
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
