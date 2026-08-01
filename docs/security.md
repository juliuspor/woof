# security

woof treats captured context, audio, credentials, and bearer tokens as sensitive.

## Local HTTP

- The daemon binds exactly to `127.0.0.1:3334`.
- `GET /health` is the only unauthenticated route.
- The daemon authenticates every other request before routing.
- The daemon compares bearer values in constant time.
- Request bodies have a fixed maximum size.
- Supervisors send a fresh 32-byte challenge to `GET /health` and accept the daemon only when its exact response includes the matching HMAC-SHA256 proof. The bearer token is never sent in a health request.

## Credentials and files

- woof stores the OpenAI API key in macOS Keychain service `com.julius.woof.openai`.
- The local bearer token is a 64-character hexadecimal value stored at `~/.woof/api-token`.
- Sensitive files use mode `0600`; private directories use mode `0700`.
- The storage layer repairs SQLite database, WAL, and shared-memory files to private permissions at startup.
- Both native processes set a private file-creation mask before creating runtime state.

## Network

Runtime network clients may contact only `https://api.openai.com` and the corresponding secure WebSocket endpoint on the same host. Those clients disable redirects and environment proxies. Local process communication stays on loopback.

Configuring an OpenAI key enables periodic memory-generation requests in addition to user-invoked chat, rewriting, and transcription. [`privacy.md`](privacy.md) lists the data classes sent by each feature.

## Capture and data lifecycle

- Capture reads text through macOS Accessibility and doesn't take screenshots.
- Capture stops during secure input and supports a persisted pause plus application exclusions.
- Local nudges and macOS notifications default to off.
- Finite retention prunes expired source data, invalidates derived memory that could retain it, and securely removes all uninspectable quarantine copies before reporting success.
- Delete-all uses SQLite secure deletion, WAL checkpoints, a vacuum, and empty index rebuilds while preserving schema version 18.

## Logging

Logs must not contain bearer tokens, OpenAI keys, captured text, or audio. Diagnostics should record state transitions and redacted error categories only.

## Process lifecycle

The desktop process supervises one daemon child and applies the persisted pause state before capture starts. Parent-stdin monitoring and shutdown hooks prevent an orphaned daemon. macOS bundle metadata prohibits multiple desktop instances.

## Release credentials

Production bundles require an Apple Developer ID Application identity, hardened runtime, and a secure timestamp; the single-file daemon and MCP helper embed signed Info.plist metadata with the stable identities `com.julius.woof.daemon` and `com.julius.woof.mcp`. These identifiers keep rebuild hashes from changing code identities and invalidating identity-bound macOS grants. The release verifier checks the identifiers, bound metadata, and designated requirements against the Apple Developer ID chain and selected Team ID.

The release build remaps Rust source prefixes to product-generic values, then rejects binaries that retain the repository or build-account path. Notarization credentials stay in a `notarytool` Keychain profile; the release script rejects inherited signing and notarization credential variables and never prints credential material. It packages only an Apple-accepted, stapled bundle that passes `stapler`, strict code-signature verification, and Gatekeeper assessment. Use self-issued signing helpers only for local development; they can't enter production releases.

Run `node scripts/audit-runtime-boundary.mjs` for source checks. With a running installed build, use `node scripts/audit-runtime-boundary.mjs --live` to verify permissions, process ownership, local authentication, listener binding, and network destinations.
