# Repository Guidelines

## Project Structure & Module Organization
- Root is a Rust workspace with two crates: `swiftide-docker-executor/` (library used by agents) and `swiftide-docker-service/` (gRPC sidecar built into the Docker image).
- Each crate has `src/` for code and `proto/` for the protobuf definitions compiled via `build.rs`.
- Executor tests live in `swiftide-docker-executor/src/tests.rs` with snapshot data under `swiftide-docker-executor/src/snapshots/`.
- Docker artifacts (Dockerfile variants) are kept alongside each crate; images are tagged from the workspace version in `Cargo.toml`.

## Build, Test, and Development Commands
- `just test` — builds the service image, then runs the full suite with `cargo nextest run --no-fail-fast` (requires a running Docker daemon).
- `cargo nextest run` — fast parallel test runner; pass `-p swiftide-docker-executor` or `-p swiftide-docker-service` to scope.
- `cargo fmt --all` — format the Rust workspace (run before committing).
- `cargo clippy --all-targets --all-features -- -D warnings` — lint; CI expects a clean run.
- `just docker-build-service` — build and tag the service image (`bosunai/swiftide-docker-service:<version>|latest`) from `swiftide-docker-service/Dockerfile`.

## Coding Style & Naming Conventions
- Rust 2024 edition; keep `rustfmt` defaults (4-space indent, trailing commas, sorted imports).
- Prefer `snake_case` for functions/modules, `CamelCase` for types, `SCREAMING_SNAKE_CASE` for constants.
- Log with `tracing`; avoid `println!` in library code.
- gRPC types are generated under `proto::*`; avoid editing generated files—change `.proto` sources instead.

## Testing Guidelines
- Primary framework: Rust tests executed via `cargo nextest`. Unit tests live beside implementations; integration helpers sit in `tests.rs`.
- Snapshot assertions use `insta`; if a legitimate change alters snapshots, review with `cargo insta review` then accept updates.
- Tests that touch Docker expect the local daemon; set `DOCKER_HOST` if using a remote socket.

## Commit & Pull Request Guidelines
- Follow Conventional Commits seen in history (`fix:`, `feat:`, `chore:`, `refactor:`, etc.); keep scope short and lower-case.
- Before opening a PR: run `cargo fmt`, `cargo clippy`, and the test suite; note any skipped tests.
- PR description should summarize the change, list test commands executed, and link the relevant issue. Include logs or screenshots when debugging Docker behavior.

## Docker & Configuration Tips
- Keep build contexts small; prefer `.dockerignore` updates over large contexts when adding assets.
- The executor supports optional BuildKit (`buildkit` feature) for faster builds; enable only when your Docker daemon supports it.
- Avoid committing secrets; rely on Docker build args or env vars provided at runtime.***
