# woof repository guidance

This file applies to the entire repository. Keep durable, cross-cutting guidance here; add a nested `AGENTS.md` only when a subtree needs genuinely narrower rules.

## Repository map and sources of truth

- `apps/woof` is the Tauri desktop application and Svelte interface.
- `apps/woof_d` is the loopback-only background daemon.
- `apps/woof-mcp` is the read-only stdio MCP server.
- `crates` contains the shared Rust libraries; `docs/contracts` contains the stable machine-readable interfaces; `scripts` contains the supported build, staging, audit, and release entry points.
- Use `README.md` for the repository overview. Use `docs/architecture.md`, `docs/security.md`, and `docs/privacy.md` for design and trust boundaries, and `docs/verification.md` for focused and release checks.
- Keep HTTP route changes synchronized across `apps/woof_d/src/lib.rs`, `docs/contracts/http.json`, `docs/contracts/backend/http-routes.json`, and the daemon contract tests.
- Keep SQLite changes synchronized across `crates/woof-storage/src/schema.sql`, `docs/contracts/backend/sqlite-v18.json`, and `crates/woof-storage/tests/schema_v18.rs`.
- Keep MCP changes synchronized across `docs/contracts/backend/mcp-tools.json`, `apps/woof-mcp`, and its protocol contract tests.
- Keep native UI IPC and geometry changes synchronized with `apps/woof/src/lib/contracts` and the corresponding tests under `apps/woof/tests`.

## Working agreements

- Make the smallest coherent change and update source, tests, and contract documentation together when behavior changes.
- Do not hand-edit ignored output under `apps/woof/.svelte-kit`, `apps/woof/build`, or `node_modules`.
- Stage sidecar binaries through `scripts/stage-sidecars.sh`; regenerate derived artwork through `node scripts/build-icons.mjs`.
- `vendor/wry` is a pinned local patch. Preserve the scope and provenance documented in `vendor/wry/woof-patch.md`; avoid unrelated vendor churn.
- Treat installed applications and user runtime data outside this repository as read-only unless the user explicitly authorizes a change.

## Build and verification

Run commands from the repository root. Install JavaScript dependencies with `npm ci` when needed.

Use focused checks while iterating:

```sh
cargo fmt --all --check
cargo test --workspace --all-targets --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
npm run check
npm test
npm run build --workspace apps/woof
node scripts/audit-runtime-boundary.mjs
```

For production-code, contract, build-tooling, or asset-pipeline changes, run the authoritative source suite before handoff:

```sh
scripts/verify.sh
```

Success ends with `woof source verification passed.` For a documentation-only change, inspect the diff; the full source suite is not required. Report every skipped or unavailable relevant check and why.

Root `npm run dev` and `npm run build` build and stage fresh debug sidecars automatically. Use `scripts/stage-sidecars.sh debug` for a stage-only refresh. Root `npm run build` builds the Tauri application; `npm run build --workspace apps/woof` builds only the web interface. The release pipeline stages release sidecars and uses the internal pre-staged build command so they are not replaced by debug binaries.

Do not use `scripts/build-release.sh` as routine verification. Run it only when release work is explicitly in scope, and invoke it directly rather than through `sh`.

## Code Review Rules

### SQLite v18 compatibility

- Preserve the SQLite schema at user version 18, including non-STRICT tables, nullable TEXT primary keys, the index name `idx_recev_session`, and the absence of a `snapshots_ad` trigger.
- Safe path: implement requested behavior without changing the frozen schema; surface any conflicting request instead of editing the contract.

### Local service boundary

- Keep every HTTP route except `GET /health` behind constant-time bearer authentication performed before routing.
- Bind only to `127.0.0.1:3334`; never bind a wildcard or alternate interface.
- Safe path: add routes behind the shared authentication boundary, retain the exact listener, and extend the existing HTTP contract tests.

### Sensitive-data boundary

- Runtime network clients may contact only `https://api.openai.com`.
- Never log bearer tokens, OpenAI keys, captured text, or audio.
- Store production OpenAI keys in macOS Keychain service `com.julius.woof.openai`.
- Create persisted sensitive files with mode `0600` and private state directories with mode `0700`.
- Safe path: reuse the pinned OpenAI clients and existing Keychain and secure-file helpers; keep diagnostics limited to redacted state and error categories, and extend the fail-closed boundary audits for any new surface.

## Product conventions

- Keep the product name lowercase: `woof`.
- Use boxer and dog visuals while preserving the established interaction geometry.
