# architecture

woof is a native macOS application with three executable components and shared Rust libraries.

## Desktop application

The Tauri process owns the menu-bar lifecycle, top-edge window, settings interface, Keychain access, and daemon supervision. It starts one daemon child, proves daemon ownership with a fresh challenge tied to the local bearer token, passes the persisted pause state before capture starts, and terminates the child during shutdown.

The Svelte interface communicates with trusted Tauri commands and the authenticated daemon API. Source-controlled window dimensions and activation behavior keep the compact and expanded states deterministic.

Accessibility onboarding requires both native clients: the Tauri process checks its own TCC state for inline rewriting, while an authenticated daemon status check verifies the independently trusted, running `woof_d` capture process. Each process invokes the macOS prompt API for its own code identity, and onboarding rechecks both after the daemon resume response before persisting completion.

## Daemon

`woof_d` owns Accessibility capture, persistence, local semantic search, generated memory, reminders, time tracking, and local HTTP routing. It binds exactly to `127.0.0.1:3334`. Only `GET /health` is public. Authentication runs before all other route matching so unknown and malformed protected requests cannot reveal route structure.

The daemon stores SQLite data at `~/Library/Application Support/woof/woof.db`. Schema user version 18 is authoritative. WAL support files share the database's private permissions. A configurable retention service prunes expired source rows and invalidates derived memory that could still contain expired content. Delete-all securely clears every logical data table, rebuilds empty search indexes, and resets local identity while leaving the usable schema in place.

If startup safely quarantines an unusable database and creates fresh storage, the daemon exposes only a bounded recovery reason through authenticated status. The desktop supervisor continues to discard sidecar stdout and stderr, then shows this structured notice without forwarding database paths or content to the UI or logs.

When the user configures an OpenAI key, a bounded scheduler periodically sends due activity context to `api.openai.com` to generate chronicles, wiki memory, actionable flags, and time-classification rules. Chat, rewriting, and Realtime transcription use the same pinned host when the user invokes them. Local capture and semantic indexing don't require OpenAI.

The local daemon evaluates reminder rules while woof is running. The menu-bar process polls ready nudges and submits an immediate generic request to macOS Notification Center whose user-info contains only an opaque nudge identifier; it doesn't register a future OS alarm. After the next launch, the daemon evaluates reminders that became due while woof was closed. Enabling Open at login provides continuity after sign-in without adding a remote push service.

Detected workflows preserve recurring local-memory patterns for later review. woof never uses workflow status to control another application or run an action.

## MCP server

`woof-mcp` is an optional stdio server. It forwards exactly ten read-only tool calls to the daemon with the local bearer token. The server compiles its public tool definitions from `docs/contracts/backend/mcp-tools.json`. Bounded frames, bounded daemon responses, strict argument schemas, and challenged health checks constrain this bridge.

## Shared libraries

The workspace crates provide configuration, secure-file handling, Accessibility capture, SQLite access, indexing, local semantic embedding, OpenAI calls, and shared API types. The workspace isolates network-capable code so audits can enumerate runtime destinations.

## Trust boundaries

1. macOS Accessibility provides user-approved foreground context.
2. The desktop process and MCP server communicate with the daemon over authenticated IPv4 loopback and accept health only with a token-bound proof.
3. Local state remains in woof-owned private directories.
4. Remote processing uses only HTTPS and WebSocket endpoints on `api.openai.com`.

No runtime component requires a second local service or a bundled external embedding payload.
