# Contributing

Contributions are welcome. This is an early Rust prototype for observable, goal-based control, with Minecraft Java 26.2 as the first live adapter. Keep product text, documentation and public descriptions in English.

## Setup

Use rustup and the toolchain pinned in `rust-toolchain.toml`. Windows requires MSVC C++ tools and the Windows SDK. Follow the [README](README.md). Begin with the offline fixture; live testing needs an authorized compatible server and, for inference, your own TypeSafe key.

```powershell
cargo run -- --fixture-preview
cargo fmt --check
cargo test
cargo check
cargo clippy --all-targets -- -D warnings
```

Never commit credentials, private server details, personal paths, private recordings, Minecraft saves or proprietary assets. Label synthetic fixtures explicitly. Keep API calls out of ordinary automated tests.

## Changes

- Discuss major adapter or architecture changes in an issue first.
- Keep pull requests focused. Explain the problem, resulting behavior and relevant validation.
- Add meaningful regression tests for behavioral changes, especially cancellation, stale responses, world transitions, rejection and recording validation.
- Preserve provenance: fixture/live, model/manual, dispatch/acceptance, acceptance/observed success.
- Update documentation when controls, setup, compatibility or contracts change.

Use the adapter/session boundary. Declare each adapter's real limitations and stop semantics; do not assume an external world can pause or restore. Avoid abstractions without a concrete second use.

## Verification

Distinguish automated checks, headless layout, native rendering, live connectivity, TypeSafe inference and game outcomes. Include relevant protocol/toolchain versions. Mocks, screenshots and successful API responses alone do not prove successful gameplay.

For live navigation, report starting conditions, requested goal, actual displacement/outcome and stop behavior. Inspect artifacts for private telemetry before sharing. Do not call untested integrations supported.

## Attribution

Credit inspirations and dependencies in [CREDITS.md](CREDITS.md). Follow [LICENSE](LICENSE) and preserve required notices for reused material. Verify third-party code and asset licenses before copying; research references do not grant blanket reuse permission.
