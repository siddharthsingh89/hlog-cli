<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://hlog.dev/static/wordmark-dark.svg">
    <img src="https://hlog.dev/static/wordmark.svg" alt="hlog" width="220">
  </picture>
</p>

# hlog CLI

The command line for [hlog.dev](https://hlog.dev), a log for humans: short, timestamped,
append-only lines about your day, formatted like software logs.

## install

macOS / Linux:

```bash
curl -fsSL https://hlog.dev/install.sh | sh
```

Windows (PowerShell):

```powershell
irm https://hlog.dev/install.ps1 | iex
```

Or download an archive from the [releases page](https://github.com/siddharthsingh89/hlog-cli/releases/latest),
or, with Rust installed:

```bash
cargo install hlog
```

## use

```bash
hlog login                    # device flow: open the URL, enter the code, approve
hlog "shipped the thing"
hlog learn "advisory locks are session-scoped"   # info debug warn error learn win
echo "from a pipe" | hlog     # one line per input line, paced at 1 line / 2 s
hlog tail -f --level LEARN
hlog feed -f
hlog grep postgres
hlog follow @maya --level LEARN
hlog unfollow @maya
hlog review                   # your week, grouped by level (LEARN and WIN first)
hlog everyone -f              # public lines from people who opted in to discovery
hlog search "#rust"           # search everyone's public lines (or a #tag)
hlog feedback "the feed is confusing"   # goes straight to the person building hlog
hlog save @maya#12            # privately save someone's public line (hlog unsave @maya#12)
hlog saved                    # your saved lines (--level LEARN, or a search word)
hlog review --post | clip     # this week's LEARN and WIN lines as post text (pbcopy on macOS)
hlog sync                     # send lines that were queued while offline
hlog logout                   # revokes the token on the server, then deletes it locally
```

**Nothing is lost offline.** If a line can't be sent (no network, server trouble, the daily limit, or not
logged in), it's saved to `queue.jsonl` next to the config and sent, oldest first, by `hlog sync` or before
your next line. Each line gets its timestamp when it reaches hlog; the CLI shows when you typed it.

- Config: `<OS config dir>/hlog/config.toml` (`~/.config/hlog/` on Linux, `~/Library/Application Support/hlog/`
  on macOS, `%APPDATA%\hlog\` on Windows), written `0600` on unix.
- `HLOG_CONFIG_DIR=/some/dir` keeps config and queue somewhere else (CI, tests, a second account).
- `HLOG_API=http://localhost:8787` points at a local server. A saved token is only sent to the server it was issued by.
- Colors only when stdout is a terminal; `NO_COLOR=1` disables them. `#tags` are highlighted.
- `hlog feed` marks where new lines start since your last look ("-- new since your last look --" / "-- caught up --").
- Once a day `hlog` checks GitHub for a newer release and prints a one-line hint; `HLOG_NO_UPDATE_CHECK=1` turns it off.
- To log text that starts with a command name: `hlog -- tail is a word too`.

Full docs: <https://hlog.dev/docs/cli>

## releasing

Bump `version` in `Cargo.toml`, commit, then `git tag v0.x.y && git push --tags`. The release workflow builds
Linux (x86_64/arm64, static musl), macOS (arm64/x86_64) and Windows (x86_64) binaries and attaches them with
SHA-256 checksums.
