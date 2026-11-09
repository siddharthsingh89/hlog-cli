//! hlog — a log for humans, from the terminal.
mod api;
mod config;
mod output;
mod queue;
mod state;

use api::{ApiError, Client, Line, DEFAULT_API};
use output::Style;
use chrono::TimeZone;
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
  hlog review [--weeks-ago N]      your week, grouped by level (LEARN and WIN first)
  hlog review --post               this week's LEARN and WIN lines as text to paste into a post
  hlog everyone [-f] [--level X]   public lines from people who opted in to discovery
  hlog search <query|#tag>         search everyone's public lines (opted-in people)
  hlog feedback \"…\" [--no-reply]   tell the person building hlog what you think
  hlog save @maya#12               privately save someone's public line (unsave @maya#12)
  hlog saved [--level X] [query]   your saved lines
  hlog follow @maya [--level LEARN]
  hlog unfollow @maya
  hlog sync                        send lines that were queued while offline
  hlog logout                      revoke this machine's token and forget it
  hlog version                     print the version (also -V, --version)
  hlog -- tail is a word too       log text that starts with a command name

env: HLOG_API (default https://hlog.dev), NO_COLOR, HLOG_CONFIG_DIR, HLOG_NO_UPDATE_CHECK";

const LEVELS: [&str; 6] = ["info", "debug", "warn", "error", "learn", "win"];
const POLL_SECS: u64 = 5;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let quiet = matches!(args.first().map(|s| s.as_str()), Some("-V") | Some("--version")) || (args.len() == 1 && args[0] == "version");
    let code = match run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("hlog: {e}");
            ExitCode::FAILURE
        }
    };
    if !quiet {
        update_notice();
    }
    code
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
        // `hlog version` on its own is what people type; with more words it's still a line ("version 2 shipped").
        Some("version") if args.len() == 1 => {
            println!("hlog {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--") => append(&ctx, &args[1..].join(" "), None),
        Some("login") => login(&ctx),
        Some("sync") => sync(&ctx),
        Some("feedback") => {
            let no_reply = args[1..].iter().any(|a| a == "--no-reply");
            let text = args[1..].iter().filter(|a| *a != "--no-reply").cloned().collect::<Vec<_>>().join(" ");
            feedback(&ctx, &text, !no_reply)
        }
        Some("review") => {
            let mut weeks = 0u32;
            let mut post = false;
            let mut it = args[1..].iter();
            while let Some(a) = it.next() {
                if a == "--post" {
                    post = true;
                } else if a == "--weeks-ago" {
                    weeks = it.next().and_then(|v| v.parse().ok()).ok_or("--weeks-ago needs a number")?;
                } else if let Some(v) = a.strip_prefix("--weeks-ago=") {
                    weeks = v.parse().map_err(|_| "--weeks-ago needs a number")?;
                } else {
                    return Err(format!("unexpected argument: {a}"));
                }
            }
            review(&ctx, weeks, post)
        }
        Some("everyone") => {
            let (follow_mode, level, rest) = flags(&args[1..])?;
            no_extra(&rest)?;
            everyone(&ctx, follow_mode, level)
        }
        Some("search") => {
            let (_, level, rest) = flags(&args[1..])?;
            let q = rest.join(" ");
            if q.trim().is_empty() {
                return Err("usage: hlog search <query>   (or a #tag)".into());
            }
            search(&ctx, &q, level)
        }
        Some("save") | Some("unsave") => {
            let usage = format!("usage: hlog {} @maya#12", args[0]);
            let r = match &args[1..] {
                [one] => parse_ref(one).ok_or(usage)?,
                _ => return Err(usage),
            };
            save_cmd(&ctx, &r, args[0] == "save")
        }
        Some("saved") => {
            let (_, level, rest) = flags(&args[1..])?;
            saved_cmd(&ctx, level, rest.join(" "))
        }
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
    check_length(text, level)?;
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
        if let Err(e) = check_length(&line, None) {
            eprintln!("skipped ({e}): {line}");
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

#[derive(Clone, Copy, PartialEq)]
enum Stream {
    Own,
    /// Your feed: remembers where you stopped reading and marks it.
    Feed,
    /// /everyone: no login needed.
    Public,
}

/// Print lines oldest-first; in the feed, mark where new lines (since your last look) begin.
fn print_with_marker(ctx: &Ctx, lines: &[Line], seen: Option<i64>) {
    for row in marker_rows(lines, seen) {
        match row {
            Row::Line(l) => println!("{}", ctx.style.line(l, true)),
            Row::NewSince => println!("{}", ctx.style.dim("-- new since your last look --")),
            Row::CaughtUp => println!("{}", ctx.style.dim("-- caught up --")),
        }
    }
}

#[derive(Debug)]
enum Row<'a> {
    Line(&'a Line),
    NewSince,
    CaughtUp,
}

/// Oldest-first rows with a divider where lines newer than `seen` begin (only if older ones were shown),
/// or "caught up" at the end when nothing is new. No marker on the very first look (`seen` = None).
fn marker_rows(lines: &[Line], seen: Option<i64>) -> Vec<Row<'_>> {
    let mut rows = Vec::new();
    let mut marked = false;
    let mut printed_old = false;
    for l in lines.iter().rev() {
        if let Some(s) = seen {
            if !marked && l.ts_ms > s {
                if printed_old {
                    rows.push(Row::NewSince);
                }
                marked = true;
            }
            if l.ts_ms <= s {
                printed_old = true;
            }
        }
        rows.push(Row::Line(l));
    }
    if seen.is_some() && !marked && !lines.is_empty() {
        rows.push(Row::CaughtUp);
    }
    rows
}

fn remember_feed(ctx: &Ctx, lines: &[Line]) {
    if let Some(newest) = lines.iter().map(|l| l.ts_ms).max() {
        let mut st = state::load();
        let e = st.feed_seen.entry(ctx.base.clone()).or_insert(0);
        if newest > *e {
            *e = newest;
            state::save(&st);
        }
    }
}

/// Shared by `tail`, `feed` and `everyone`: print the latest page, then optionally poll for newer lines.
fn stream(ctx: &Ctx, path: &str, base_query: Vec<(&str, String)>, follow_mode: bool, kind: Stream) -> Result<(), String> {
    let who = kind != Stream::Own;
    let client = if kind == Stream::Public { ctx.client() } else { ctx.authed()? };
    let mut q = base_query.clone();
    q.push(("limit", "20".into()));
    let page = client.page(path, &q).map_err(|e| e.to_string())?;
    if kind == Stream::Feed {
        let seen = state::load().feed_seen.get(&ctx.base).copied();
        print_with_marker(ctx, &page.lines, seen);
        remember_feed(ctx, &page.lines);
    } else {
        print_oldest_first(ctx, &page.lines, who);
    }
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
                if kind == Stream::Feed {
                    remember_feed(ctx, &p.lines);
                }
            }
            Err(ApiError::Network(_)) => continue, // keep tailing through blips
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn tail(ctx: &Ctx, follow_mode: bool, level: Option<String>) -> Result<(), String> {
    let q = level.map(|l| vec![("level", l)]).unwrap_or_default();
    stream(ctx, "/lines", q, follow_mode, Stream::Own)
}

fn feed(ctx: &Ctx, follow_mode: bool) -> Result<(), String> {
    stream(ctx, "/feed", vec![], follow_mode, Stream::Feed)
}

fn everyone(ctx: &Ctx, follow_mode: bool, level: Option<String>) -> Result<(), String> {
    let q = level.map(|l| vec![("level", l)]).unwrap_or_default();
    stream(ctx, "/everyone", q, follow_mode, Stream::Public)
}

fn search(ctx: &Ctx, q: &str, level: Option<String>) -> Result<(), String> {
    let mut query = vec![("q", q.to_string())];
    if let Some(l) = level {
        query.push(("level", l));
    }
    let page = ctx.client().page("/search", &query).map_err(|e| e.to_string())?;
    if page.lines.is_empty() {
        eprintln!("no public lines match (only people who opted in to discovery are searchable)");
    }
    print_oldest_first(ctx, &page.lines, true);
    Ok(())
}

fn feedback(ctx: &Ctx, text: &str, reply: bool) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("usage: hlog feedback \"what's confusing, broken, missing, or great\"".into());
    }
    if ctx.token.is_none() {
        return Err("log in first (`hlog login`), or use https://hlog.dev/feedback".into());
    }
    ctx.client().post("/feedback", serde_json::json!({ "text": text, "reply": reply })).map_err(|e| e.to_string())?;
    println!("thanks — sent to the person building hlog.{}", if reply { " they may reply by email." } else { "" });
    Ok(())
}

const REVIEW_ORDER: [&str; 6] = ["LEARN", "WIN", "INFO", "WARN", "ERROR", "DEBUG"];

/// Your week (rolling 7 days, local time), grouped by level, built from your own lines.
fn review(ctx: &Ctx, weeks_ago: u32, post: bool) -> Result<(), String> {
    let client = ctx.authed()?;
    let day = 86_400_000i64;
    let to = chrono::Utc::now().timestamp_millis() - weeks_ago as i64 * 7 * day;
    let from = to - 7 * day;
    let since = chrono::DateTime::from_timestamp_millis(from).map(|d| d.to_rfc3339()).unwrap_or_default();
    let mut before = format!("{to}-0");
    let mut lines: Vec<Line> = Vec::new();
    for _ in 0..20 {
        let page = client
            .page("/lines", &[("since", since.clone()), ("before", before.clone()), ("limit", "200".into())])
            .map_err(|e| e.to_string())?;
        lines.extend(page.lines.into_iter().filter(|l| l.level != "SYSTEM"));
        match page.next {
            Some(n) => before = n,
            None => break,
        }
    }
    if post {
        let mut sorted: Vec<&Line> = lines.iter().collect();
        sorted.sort_by_key(|l| l.ts_ms);
        let pick = |lvl: &str| sorted.iter().filter(|l| l.level == lvl).map(|l| l.text.clone()).take(POST_MAX).collect::<Vec<_>>();
        let me = client.get("/me", &[]).map_err(|e| e.to_string())?;
        let shares = me["public_levels"].as_array().is_some_and(|a| !a.is_empty());
        let link = match (shares, me["handle"].as_str()) {
            (true, Some(h)) => Some(format!("{}/@{h}", ctx.base.trim_end_matches('/').trim_start_matches("https://").trim_start_matches("http://"))),
            _ => None,
        };
        return match post_text(&pick("LEARN"), &pick("WIN"), link.as_deref()) {
            Some(t) => {
                println!("{t}");
                Ok(())
            }
            None => Err("no LEARN or WIN lines this week to post".into()),
        };
    }
    let local = |ms: i64| chrono::Local.timestamp_millis_opt(ms).single();
    let label = |ms: i64| local(ms).map(|d| d.format("%-d %b").to_string().to_lowercase()).unwrap_or_default();
    let days: std::collections::BTreeSet<String> = lines.iter().filter_map(|l| local(l.ts_ms).map(|d| d.format("%F").to_string())).collect();
    println!(
        "{} {} – {} · logged on {} of 7 days · {} line{}",
        if weeks_ago == 0 { "your week:" } else { "week of" },
        label(from),
        label(to - 1),
        days.len(),
        lines.len(),
        if lines.len() == 1 { "" } else { "s" }
    );
    if lines.is_empty() {
        println!("{}", ctx.style.dim("nothing logged. one line is enough to start: hlog learn \"…\""));
        return Ok(());
    }
    for lvl in REVIEW_ORDER {
        let mut group: Vec<&Line> = lines.iter().filter(|l| l.level == lvl).collect();
        if group.is_empty() {
            continue;
        }
        group.sort_by_key(|l| l.ts_ms);
        println!();
        println!("{}", ctx.style.dim(&format!("## {lvl} ({})", group.len())));
        for l in group {
            println!("{}", ctx.style.line(l, false));
        }
    }
    Ok(())
}

/// At most once a day, and only on a terminal: tell the user if a newer release exists.
fn update_notice() {
    if std::env::var_os("HLOG_NO_UPDATE_CHECK").is_some() || !std::io::stderr().is_terminal() {
        return;
    }
    let now = chrono::Utc::now().timestamp_millis();
    let mut st = state::load();
    if st.update_checked_ms.is_some_and(|t| now - t < 86_400_000) {
        return;
    }
    st.update_checked_ms = Some(now);
    let latest = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(2))
        .user_agent(concat!("hlog-cli/", env!("CARGO_PKG_VERSION")))
        .build()
        .get("https://api.github.com/repos/siddharthsingh89/hlog-cli/releases/latest")
        .call()
        .ok()
        .and_then(|r| r.into_json::<serde_json::Value>().ok())
        .and_then(|v| v["tag_name"].as_str().map(str::to_string));
    if let Some(l) = latest {
        if state::newer(&l, env!("CARGO_PKG_VERSION")) {
            let installer = if cfg!(windows) { "irm https://hlog.dev/install.ps1 | iex" } else { "curl -fsSL https://hlog.dev/install.sh | sh" };
            eprintln!(
                "\nhlog {} is available (you have {}). update: {installer}  ·  cargo install hlog",
                l.trim_start_matches('v'),
                env!("CARGO_PKG_VERSION")
            );
        }
        st.latest_version = Some(l);
    }
    state::save(&st);
}


/// Same limit as the server (lines/levels.ts), counted the same way, so an over-long line fails now
/// instead of sitting in the offline queue and failing at `hlog sync`.
const MAX_LINE_CHARS: usize = 500;
const POST_MAX: usize = 10;

/// Characters the server will count: line breaks (CR, LF, U+2028, U+2029) collapse to one space, tabs
/// become spaces, ends trimmed, and a leading level word ("WARN …") doesn't count when no level is given.
fn line_length(text: &str, level: Option<&str>) -> usize {
    let mut norm = String::with_capacity(text.len());
    let mut in_break = false;
    for c in text.chars() {
        if matches!(c as u32, 0x0D | 0x0A | 0x2028 | 0x2029) {
            if !in_break {
                norm.push(' ');
            }
            in_break = true;
            continue;
        }
        in_break = false;
        norm.push(if c == '\t' { ' ' } else { c });
    }
    let mut t = norm.trim();
    if level.is_none() {
        if let Some((word, rest)) = t.split_once(' ') {
            if LEVELS.contains(&word.to_lowercase().as_str()) {
                t = rest.trim_start_matches(' ');
            }
        }
    }
    t.chars().count()
}

fn check_length(text: &str, level: Option<&str>) -> Result<(), String> {
    let n = line_length(text, level);
    if n > MAX_LINE_CHARS {
        return Err(format!("line is longer than {MAX_LINE_CHARS} characters ({} over)", n - MAX_LINE_CHARS));
    }
    Ok(())
}

/// The same text as "copy as post" on the web review page: your own lines, word for word.
fn post_text(learn: &[String], win: &[String], link: Option<&str>) -> Option<String> {
    if learn.is_empty() && win.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if !learn.is_empty() {
        parts.push(std::iter::once("what I learned this week:".to_string()).chain(learn.iter().map(|t| format!("– {t}"))).collect::<Vec<_>>().join("\n"));
    }
    if !win.is_empty() {
        let head = if learn.is_empty() { "what I shipped this week:" } else { "and shipped:" };
        parts.push(std::iter::once(head.to_string()).chain(win.iter().map(|t| format!("– {t}"))).collect::<Vec<_>>().join("\n"));
    }
    if let Some(l) = link {
        parts.push(format!("my log: {l}"));
    }
    Some(parts.join("\n\n"))
}

/// "@maya#12" or "maya#12" -> ("maya", 12)
fn parse_ref(s: &str) -> Option<(String, u64)> {
    let (h, n) = s.trim().trim_start_matches('@').split_once('#')?;
    let handle_ok = h.len() >= 3
        && h.len() <= 20
        && h.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let seq: u64 = n.parse().ok().filter(|&n| n > 0)?;
    handle_ok.then(|| (h.to_lowercase(), seq))
}

fn save_cmd(ctx: &Ctx, (handle, seq): &(String, u64), on: bool) -> Result<(), String> {
    let client = ctx.authed()?;
    if on {
        client.post("/saves", serde_json::json!({ "ref": format!("@{handle}#{seq}") })).map_err(|e| e.to_string())?;
        println!("saved @{handle}#{seq} (private; `hlog saved` lists your saved lines)");
    } else {
        client.delete(&format!("/saves/{handle}/{seq}")).map_err(|e| e.to_string())?;
        println!("unsaved @{handle}#{seq}");
    }
    Ok(())
}

fn saved_cmd(ctx: &Ctx, level: Option<String>, q: String) -> Result<(), String> {
    let mut query = vec![("limit", "100".to_string())];
    if let Some(l) = level {
        query.push(("level", l));
    }
    if !q.trim().is_empty() {
        query.push(("q", q));
    }
    let v = ctx.authed()?.get("/saved", &query).map_err(|e| e.to_string())?;
    let items = v["items"].as_array().cloned().unwrap_or_default();
    if items.is_empty() {
        eprintln!("nothing saved (save someone's public line with `hlog save @maya#12`)");
        return Ok(());
    }
    // Newest saved at the bottom, like tail.
    for it in items.iter().rev() {
        match serde_json::from_value::<Line>(it["line"].clone()) {
            Ok(l) => println!("{}", ctx.style.line(&l, true)),
            Err(_) => {
                let r = it["ref"].as_str().unwrap_or("?");
                println!("{}", ctx.style.dim(&format!("{r} is no longer available (remove it: hlog unsave {r})")));
            }
        }
    }
    Ok(())
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
    #[test]
    fn line_length_matches_the_server() {
        use super::line_length;
        assert_eq!(line_length("  hello  ", None), 5);
        assert_eq!(line_length("WARN disk full", None), 9);
        assert_eq!(line_length("WARN disk full", Some("INFO")), 14);
        assert_eq!(line_length("a\r\n\nb", None), 3);
        assert_eq!(line_length("a\tb", None), 3);
        let sep: String = [char::from_u32(0x2028).unwrap(), char::from_u32(0x2029).unwrap()].iter().collect();
        assert_eq!(line_length(&format!("a{sep}b"), None), 3);
        assert_eq!(line_length(&"😀".repeat(500), None), 500);
        assert!(super::check_length(&"x".repeat(501), None).unwrap_err().contains("1 over"));
        assert!(super::check_length(&format!("LEARN {}", "x".repeat(500)), None).is_ok());
    }

    #[test]
    fn post_text_matches_the_web() {
        use super::post_text;
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            post_text(&s(&["a", "b"]), &s(&["shipped c"]), Some("hlog.dev/@sid")).unwrap(),
            "what I learned this week:\n– a\n– b\n\nand shipped:\n– shipped c\n\nmy log: hlog.dev/@sid"
        );
        assert_eq!(post_text(&[], &s(&["w"]), None).unwrap(), "what I shipped this week:\n– w");
        assert!(post_text(&[], &[], Some("x")).is_none());
    }

    #[test]
    fn parses_line_refs() {
        use super::parse_ref;
        assert_eq!(parse_ref("@Maya#12"), Some(("maya".into(), 12)));
        assert_eq!(parse_ref("maya#3"), Some(("maya".into(), 3)));
        assert_eq!(parse_ref("@ma#1"), None);
        assert_eq!(parse_ref("@maya#0"), None);
        assert_eq!(parse_ref("@maya"), None);
        assert_eq!(parse_ref("@1maya#2"), None);
    }

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

    fn line(ts: i64) -> Line {
        Line { handle: "maya".into(), ts_ms: ts, level: "INFO".into(), text: format!("t{ts}"), cursor: format!("{ts}-1") }
    }

    #[test]
    fn feed_marker_positions() {
        // API order is newest first
        let lines = vec![line(30), line(20), line(10)];
        let kinds = |rows: Vec<Row>| rows.iter().map(|r| match r { Row::Line(l) => l.ts_ms.to_string(), Row::NewSince => "NEW".into(), Row::CaughtUp => "UP".into() }).collect::<Vec<_>>().join(",");
        assert_eq!(kinds(marker_rows(&lines, None)), "10,20,30"); // first look: no marker
        assert_eq!(kinds(marker_rows(&lines, Some(15))), "10,NEW,20,30");
        assert_eq!(kinds(marker_rows(&lines, Some(30))), "10,20,30,UP");
        assert_eq!(kinds(marker_rows(&lines, Some(5))), "10,20,30"); // all new, nothing older shown
    }

    #[test]
    fn handles_strip_at() {
        assert_eq!(one_handle(&s(&["@maya"]), "follow").unwrap(), "maya");
        assert!(one_handle(&s(&[]), "follow").is_err());
        assert!(one_handle(&s(&["a", "b"]), "follow").is_err());
    }
}
