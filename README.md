# hlog CLI

```bash
cargo install --path .        # or: cargo build --release  ->  target/release/hlog
hlog login                    # device flow: open the URL, enter the code, approve
hlog "shipped the thing"
hlog learn "advisory locks are session-scoped"
echo "from a pipe" | hlog     # one line per input line, paced at 1 line / 2 s
hlog tail -f --level LEARN
hlog feed -f
hlog grep postgres
hlog follow @maya --level LEARN
hlog unfollow @maya
hlog logout                   # revokes the token on the server, then deletes it locally
```

- Config: `<OS config dir>/hlog/config.toml` (`~/.config/hlog/config.toml` on Linux,
  `~/Library/Application Support/hlog/` on macOS, `%APPDATA%\hlog\` on Windows), written `0600` on unix.
- `HLOG_API=http://localhost:8787` points at local dev. A saved token is only sent to the server it was
  issued by.
- Colors only when stdout is a terminal; `NO_COLOR=1` disables them.
- To log text that starts with a command name: `hlog -- tail is a word too`.
