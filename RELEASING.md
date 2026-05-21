# Releasing

Small checklist. Do not make release while tired and sleepy. Future you will suffer.

## Check First

Run:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --locked
cargo build --locked --release
./target/release/tgbacky --version
```

Then check:

- README commands still match CLI
- `env.example` has all config env vars
- `CHANGELOG.md` says what changed
- no `.env`
- no session DB
- no state DB
- no downloads
- no `.part` files
- no local credentials

## Version

Update:

- `Cargo.toml`
- `CHANGELOG.md`

Tag:

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

## GitHub Release

Workflow: [`.github/workflows/release.yml`](.github/workflows/release.yml)

Pushing `v*` tag does this:

- run format, clippy, tests
- build release binaries
- run `tgbacky --version` on each package
- build Linux, macOS Intel, macOS Apple Silicon, and Windows archives
- include `README.md`, `LICENSE`, and `env.example`
- write `SHA256SUMS.txt`
- publish GitHub Release
