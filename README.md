# tgbacky

`tgbacky` saves media from Telegram chats to local folders.

It uses your normal Telegram account. It keeps a small local SQLite DB so it can
resume later instead of starting from zero.

It can save:

- photos
- image documents
- videos
- animations
- audio
- voice notes
- normal documents

Files sent as files stay files. `tgbacky` does not convert PNG to JPG, WebP to
PNG, video to MP4, or anything like that. It saves Telegram bytes as-is.

Not here yet: bot tokens, invite links, stickers, profile photos, contacts, and
multi-chat batch export.

## Install

From this checkout:

```bash
cargo install --path .
tgbacky --version
```

Or build without installing:

```bash
cargo build --release
./target/release/tgbacky --version
```

Release archives contain `tgbacky`, `README.md`, `LICENSE`, and `env.example`.
If `SHA256SUMS.txt` exists, check it before running downloaded binaries.

## First Use

Login once:

```bash
tgbacky auth
```

First run asks for API ID and API hash once. They are app/client credentials,
not Telegram user credentials. All Telegram account profiles can reuse same API
credentials.

Get API ID and API hash from
[my.telegram.org](https://my.telegram.org):

1. Sign in.
2. Open `API development tools`.
3. Create an app.
4. Copy `api_id` and `api_hash`.

List chats:

```bash
tgbacky chats list
```

Pick a chat id, username, or exact title. Numeric id is safest.

Export one chat:

```bash
tgbacky export --chat -1001234567890 --out ./downloads
```

Run same command again later. It resumes.

## Common Commands

Use another profile:

```bash
tgbacky auth --profile work
tgbacky chats list --profile work
tgbacky export --profile work --chat @example --out ./downloads/work
```

Show profiles:

```bash
tgbacky profiles list
tgbacky profiles current
tgbacky profiles use work
```

Manage API credentials:

```bash
tgbacky api list
tgbacky api add --name default
tgbacky api add --name backup --api-id 123456 --api-hash abcdef
tgbacky api use backup
tgbacky api delete backup --yes
```

Use one API profile for one command:

```bash
tgbacky auth --profile work --api-profile backup
```

Export only some media:

```bash
tgbacky export --chat @example --out ./downloads --only photo,video
tgbacky export --chat @example --out ./downloads --skip document,audio,voice
```

Force rescan:

```bash
tgbacky export --chat @example --out ./downloads --rescan
```

Plan first, download later:

```bash
tgbacky export plan --chat @example --out ./downloads --save-queue
tgbacky export --chat @example --out ./downloads
```

Verify files:

```bash
tgbacky verify --chat @example --out ./downloads
tgbacky verify --chat @example --out ./downloads --deep
tgbacky verify --chat -1001234567890 --out ./downloads --deep --json
```

Check local setup:

```bash
tgbacky doctor
tgbacky doctor --live
```

See previous runs:

```bash
tgbacky runs list --limit 20
tgbacky runs list --failed-only
```

Find old `.part` files:

```bash
tgbacky recover stale-parts --out ./downloads
tgbacky recover stale-parts --out ./downloads --delete
```

Reset one chat in local DB:

```bash
tgbacky chats reset --chat @example --keep-files --yes
```

Remove `--keep-files` if you also want to delete tracked files.

## Useful Export Flags

```text
--chat <CHAT>                      username, exact title, or numeric chat id
--out <DIR>                        where files go
--only <KINDS>                     save only these media kinds
--skip <KINDS>                     skip these media kinds
--workers <N>                      download workers
--rescan                           scan history again
--verbose-progress                 show more live details
--json-report                      print JSON report after run
```

Media kinds:

```text
photo,image_doc,video,animation,audio,voice,document
```

Runtime flags:

```text
--delay-ms <MS>
--flood-sleep-threshold-secs <SECS>
--jitter-ms <MS>
--retry-count <N>
--retry-backoff-ms <MS>
--download-stall-timeout-secs <SECS>
```

Example for gentle but not too slow export:

```bash
tgbacky export \
  --chat @example \
  --out ./downloads \
  --workers 4 \
  --delay-ms 700 \
  --retry-count 5 \
  --download-stall-timeout-secs 120 \
  --verbose-progress
```

## Where Files Go

Files go under output dir, then chat folder, then media type and date:

```text
downloads/
  my_chat/
    photos/2026/04/...
    videos/2026/04/...
    audio/2026/04/...
    files/2026/04/...
    animations/2026/04/...
```

State DB is separate from media files.

Default profile data lives under your OS app-data folder. You can override paths:

```bash
tgbacky auth \
  --profile work \
  --session ./data/work.session.db \
  --db ./data/work.state.db \
  --download-dir ./downloads/work \
  --artifacts-dir ./data/work-artifacts
```

Important paths:

```text
tgbacky_API_PROFILE          default API credential set name
tgbacky_SESSION_PATH        Telegram login/session DB
tgbacky_DB_PATH             chat state, checkpoints, media records, run history
tgbacky_DOWNLOAD_DIR        default output folder
tgbacky_RUN_ARTIFACT_DIR    JSON run artifacts
```

Most people can skip `.env` and use CLI flags.

Config order:

1. CLI flags
2. environment variables
3. profile defaults

See [env.example](env.example) for all env vars.

## Secrets

Keep these private:

- Telegram session DB
- local API credential fallback file
- `.env` with API ID/API hash
- downloaded private media
- state DB if chat names or file paths are private

`tgbacky` tries OS keychain first for API credentials. If keychain fails, it asks
before writing local plaintext credentials. API credentials are global, not
stored inside each Telegram account profile.

On macOS, Keychain may ask whether `tgbacky` can read saved API credentials.
Choose **Always Allow** if you trust this `tgbacky` binary. If you rebuild or
run an unsigned/dev binary, macOS may ask again because it sees a changed tool.
This is normal macOS behavior, not data loss.

Do not commit session files, state DBs, `.env`, or downloads.

More notes: [SECURITY.md](SECURITY.md).

## If Things Feel Stuck

Use:

```bash
tgbacky export --chat @example --out ./downloads --verbose-progress
```

Look at:

- `active=0`: no download running
- `active=1`: one download running or stuck
- `cooldown=yes`: Telegram asked tool to wait
- `pending=N`: queued downloads waiting
- `failed=N`: files failed after retries

If a download stalls, default timeout is 120 seconds. Change it:

```bash
tgbacky export --chat @example --out ./downloads --download-stall-timeout-secs 60
```

## Development

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo build --release
```

Release notes live in [CHANGELOG.md](CHANGELOG.md).

License: [MIT](LICENSE).
