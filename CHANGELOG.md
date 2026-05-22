# Changelog

Plain release notes for `tgbacky`.

## Unreleased

- support repeatable `--chat` on `tgbacky export` for sequential multi-chat backups

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
