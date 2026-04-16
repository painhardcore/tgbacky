# Security

`tgbacky` touches private Telegram data. Treat its local files like secrets.

## Supported Version

Security fixes target latest release on default branch.

| Version | Supported |
| --- | --- |
| 0.1.x | Yes |
| older | No |

## Report A Problem

Do not post secrets in public issues.

Good report includes:

- `tgbacky` version
- OS
- what command you ran
- what went wrong
- whether it touches session files, credentials, profiles, or private media
- logs with secrets removed

Use repository private security reporting if available. If not available, open a
small public issue with no exploit details and ask for private handoff.

## Do Not Share

Remove these before posting logs or screenshots:

- phone numbers
- login codes
- 2FA hints
- API ID and API hash
- `session.db`
- `state.db`
- API credential JSON files
- `.env`
- `.part` files
- private chat names
- private message text
- downloaded private media

## Local Files

Sensitive by design:

- Telegram session DB
- state DB with chat/media metadata
- local API credential fallback file, if keychain was unavailable

On shared machines, keep profile folder and API credential folder private.

## macOS Keychain

macOS may ask for permission when `tgbacky` reads API credentials from Keychain.
If you trust this binary, choose **Always Allow**.

Unsigned or freshly rebuilt binaries can trigger the prompt again. macOS treats
the changed binary as a new requester. This is normal; credentials are still in
Keychain.
