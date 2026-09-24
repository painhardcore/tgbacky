# tgbacky

`tgbacky` downloads media from your Telegram chats into local folders.

It logs in as your normal Telegram account, not a bot. Progress lives in a
small SQLite database, so you can stop an export and run the same command
later to pick up where it left off.

It saves photos, image documents, videos, animations, audio, voice notes, and
regular documents. Files are written exactly as Telegram stores them. A PNG
stays a PNG and a WebP stays a WebP. Nothing gets re-encoded.

Not supported yet: bot tokens, invite links, stickers, profile photos, and
contacts.

## Installation

Grab a binary from
[GitHub Releases](https://github.com/painhardcore/TgMediaBacky/releases). You
don't need Rust.

```text
Linux:               tgbacky-v0.2.0-linux-x86_64.tar.gz
macOS Apple Silicon: tgbacky-v0.2.0-macos-aarch64.tar.gz
macOS Intel:         tgbacky-v0.2.0-macos-x86_64.tar.gz
Windows:             tgbacky-v0.2.0-windows-x86_64.zip
```

Each release has a `SHA256SUMS.txt`. Check your download against it before
running the binary.

Linux:

```bash
tar -xzf tgbacky-v0.2.0-linux-x86_64.tar.gz
cd tgbacky-v0.2.0-linux-x86_64
sudo mv tgbacky /usr/local/bin/tgbacky
tgbacky --version
```

macOS on Apple Silicon. On Intel, swap `macos-aarch64` for `macos-x86_64`.

```bash
tar -xzf tgbacky-v0.2.0-macos-aarch64.tar.gz
cd tgbacky-v0.2.0-macos-aarch64
xattr -d com.apple.quarantine ./tgbacky
sudo mv tgbacky /usr/local/bin/tgbacky
tgbacky --version
```

The `xattr` line clears the quarantine flag macOS puts on downloaded files. If
macOS still blocks the binary, allow it in **System Settings > Privacy &
Security**.

Windows PowerShell:

```powershell
Expand-Archive .\tgbacky-v0.2.0-windows-x86_64.zip
cd .\tgbacky-v0.2.0-windows-x86_64\tgbacky-v0.2.0-windows-x86_64
.\tgbacky.exe --version
```

To run it from anywhere, move `tgbacky.exe` into a folder on your `Path`.

Each archive holds the binary, `README.md`, `LICENSE`, and `env.example`.

### Build from source

Needs Rust 1.85 or newer.

```bash
cargo install --path .
tgbacky --version
```

## First run

Log in:

```bash
tgbacky auth
```

The first time, `tgbacky` asks for an API ID and API hash. These identify the
app, not your account, so every account profile can share one pair. To get
them:

1. Sign in at [my.telegram.org](https://my.telegram.org).
2. Open **API development tools** and create an app.
3. Copy `api_id` and `api_hash`.

Find the chat you want:

```bash
tgbacky chats list
```

You can pass a chat as a numeric id, a `@username`, or its exact title. Use
the numeric id when you can. Titles aren't unique, and looking one up means
paging through every dialog you have.

Export it:

```bash
tgbacky export --chat -1001234567890 --out ./downloads
```

Run the same command again later to resume or to fetch new messages.

To export several chats, repeat `--chat`:

```bash
tgbacky export --chat @me --chat -1001234567890 --chat "Family Photos" --out ./downloads
```

Chats run one at a time. If one fails, the rest still run. At the end you get
a per-chat summary, and the exit code is non-zero if any chat failed.

## Everyday commands

Separate Telegram accounts with profiles:

```bash
tgbacky auth --profile work
tgbacky export --profile work --chat @example --out ./downloads/work
tgbacky profiles list
tgbacky profiles current
tgbacky profiles use work
```

Manage API credentials:

```bash
tgbacky api list
tgbacky api add --name backup --api-id 123456 --api-hash abcdef
tgbacky api use backup
tgbacky api delete backup --yes
tgbacky auth --profile work --api-profile backup   # one-off override
```

Choose which media to save:

```bash
tgbacky export --chat @example --out ./downloads --only photo,video
tgbacky export --chat @example --out ./downloads --skip document,audio,voice
```

Kinds: `photo`, `image_doc`, `video`, `animation`, `audio`, `voice`,
`document`.

Scan the whole history again, ignoring the saved checkpoint:

```bash
tgbacky export --chat @example --out ./downloads --rescan
```

See what an export would download before running it. `--save-queue` stores
the result, and the next `export` downloads that queue first:

```bash
tgbacky export plan --chat @example --out ./downloads --save-queue
tgbacky export --chat @example --out ./downloads
```

Check downloaded files:

```bash
tgbacky verify --chat @example --out ./downloads
tgbacky verify --chat @example --out ./downloads --deep --json
```

Plain `verify` compares status, paths, and file sizes against the database.
`--deep` also re-reads each file and checks its SHA-256. `--json` prints
output for scripts.

Other commands:

```bash
tgbacky doctor                 # check local setup
tgbacky doctor --live          # also talk to Telegram
tgbacky runs list --limit 20   # past runs
tgbacky runs list --failed-only
tgbacky recover stale-parts --out ./downloads            # list leftover .part files
tgbacky recover stale-parts --out ./downloads --delete
tgbacky chats reset --chat @example --keep-files --yes   # forget a chat's state
```

Without `--keep-files`, `chats reset` also deletes the files it tracked.

## Export flags

```text
--chat <CHAT>                      repeatable; numeric id, @username, or exact title
--out <DIR>                        output folder
--only <KINDS> / --skip <KINDS>    media kinds to include or exclude
--since-id <ID> / --until-id <ID>  message id range
--date-from <DATE> / --date-to <DATE>  date range, YYYY-MM-DD
--limit <N>                        stop after N messages
--workers <N>                      max parallel downloads (default: CPU count)
--rescan                           ignore the checkpoint and scan everything
--verbose-progress                 more detail in the progress line
--json-report                      print a JSON report at the end
```

Exports bounded by `--since-id`, `--until-id`, `--date-from`, or `--date-to`
leave the chat's checkpoint alone.

Pacing and retries:

```text
--delay-ms <MS>                       minimum gap between Telegram requests
--jitter-ms <MS>                      random extra delay per request
--flood-sleep-threshold-secs <SECS>   abort if Telegram asks to wait longer (0 = always wait)
--retry-count <N>                     retries per file
--retry-backoff-ms <MS>               first retry delay, doubles each time
--download-stall-timeout-secs <SECS>  give up on a download with no data for this long
```

A slower, steadier setup:

```bash
tgbacky export --chat @example --out ./downloads \
  --workers 4 --delay-ms 700 --retry-count 5 \
  --download-stall-timeout-secs 120 --verbose-progress
```

## Rate limits on large chats

Telegram throttles big exports. A chat with 100+ GB of media will hit
`FLOOD_WAIT_X` or `FLOOD_PREMIUM_WAIT_X` errors, where `X` is how many seconds
Telegram wants you to pause. `tgbacky` waits that long, drops to one download
at a time, and continues. Partial files are kept intact. Expect a very large
chat to take hours.

## Where files go

```text
downloads/
  my_chat/
    photos/2026/04/...
    videos/2026/04/...
    audio/2026/04/...
    files/2026/04/...
    animations/2026/04/...
```

The chat folder is a slug of the chat title. Year and month come from the
message date.

The session and state databases live in your OS app-data folder, apart from
the media. To put them elsewhere:

```bash
tgbacky auth --profile work \
  --session ./data/work.session.db \
  --db ./data/work.state.db \
  --download-dir ./downloads/work \
  --artifacts-dir ./data/work-artifacts
```

The same settings exist as environment variables:

```text
tgbacky_API_PROFILE        default API credential set
tgbacky_SESSION_PATH       Telegram login session
tgbacky_DB_PATH            checkpoints, media records, run history
tgbacky_DOWNLOAD_DIR       default output folder
tgbacky_RUN_ARTIFACT_DIR   per-run JSON reports
```

Precedence: CLI flags, then environment variables, then profile defaults.
[env.example](env.example) lists every variable. You don't need a `.env` file
unless you want one.

## Secrets

Treat these as private:

- the Telegram session DB, which logs in as you for whoever holds it
- the plaintext API credential file, if you allowed one
- `.env`, if it holds your API ID and hash
- the state DB, which contains chat names and file paths
- the downloaded media

API credentials go to the OS keychain first. If the keychain fails,
`tgbacky` asks before writing them to a plaintext file.

On macOS, Keychain may ask whether `tgbacky` can read them. Choose **Always
Allow** if you trust the binary. After a rebuild macOS will ask again, because
it treats the new binary as a different program.

Don't commit session files, state DBs, `.env`, or downloads. See
[SECURITY.md](SECURITY.md) for more.

## When it looks stuck

Add `--verbose-progress` and read the status line:

- `active=0`: nothing downloading
- `active=1`: one download running, or stalled
- `cooldown=yes`: Telegram asked for a pause and `tgbacky` is waiting it out
- `pending=N`: downloads queued
- `failed=N`: files that failed after all retries

A download with no new data for 120 seconds counts as stalled and gets
retried. Change the limit with `--download-stall-timeout-secs`.

## Development

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

[CHANGELOG.md](CHANGELOG.md) · [MIT License](LICENSE)
