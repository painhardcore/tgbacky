# Changelog

Plain release notes for `tgbacky`.

## Unreleased

Fixes:

- stopping an export while it scans new messages no longer skips the unscanned ones forever
- a saved `export plan --save-queue` no longer resets the checkpoint on every later run
- a file that fails in the final retry sweep counts as one failure, not two
- media paths are stored as absolute paths, so `export`, `verify`, and `chats reset` work from any directory
- session DB, state DB, and the local credentials file are created owner-only (0600) and existing ones are tightened
- GIF files sent as documents are saved as `animation`, not `image_doc`
- `original_if_available` file names keep non-Latin letters
- one Ctrl+C in a batch export prints the cancel message once

Changes:

- `--workers` defaults to 4 instead of the CPU count
- export scans trust a tracked file that exists with the recorded size; use `verify --deep` to check hashes
- `export plan` output drops `already_tracked` and `already_downloaded`, which always matched `skipped_existing`
- stale `.part` files are only scanned when start-up cleanup is enabled
- minimum Rust version is 1.88

Faster:

- `chats list` and title lookups pace once per page of 100 chats instead of per chat
- `--date-to` exports start reading history at the date instead of the newest message

## 0.2.0

- support repeatable `--chat` on `tgbacky export` for sequential multi-chat backups
- keep one run record and JSON artifact per chat during batch export
- continue remaining chats after normal per-chat failures and return non-zero if any chat failed

## 0.1.2

- resolve numeric Telegram chat IDs from the existing session peer cache before falling back to a full dialog scan
- fall back to dialog scanning when a cached numeric peer cannot be resolved

## 0.1.1

- allow negative Telegram chat IDs in CLI parsing

## 0.1.0

First public release.

What works:

- login with Telegram user account
- keep more than one profile
- keep API credentials globally, reusable by all Telegram account profiles
- manage API credentials with `tgbacky api list/add/use/delete`
- store API credentials in OS keychain when possible
- use local credential file only after user agrees
- list chats and show chat ids
- export one chat to local folders
- resume from SQLite checkpoints
- retry failed downloads later
- save photos, videos, audio, voice, animations, image documents, and files
- preserve downloaded file bytes instead of converting formats
- filter media with `--only` and `--skip`
- write run history and JSON artifacts
- find/remove stale `.part` files
- tune delays, retries, workers, and stall timeout
- verify saved files
- run CI checks: format, clippy, tests, release builds
