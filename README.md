# woof

woof is a private macOS 14+ memory companion for Apple silicon. With the user's Accessibility permission, it reads visible application context, builds a searchable local activity history, and makes that history available through its desktop interface and a read-only MCP server.

The repository contains the application source, shared libraries, assets, interface specifications, tests, and release tooling.

## Capabilities

- Captures foreground application text and focused-element context without taking screenshots.
- Stores snapshots, activity events, summaries, wiki pages, reminders, and time records on the Mac.
- Runs full-text search, semantic embeddings, and vector search locally.
- Schedules one-time and daily reminders while the menu-bar app is running.
- Detects recurring work patterns for review. Keeping a pattern doesn't execute actions or automation.
- Uses OpenAI only for periodic memory generation and user-invoked chat, rewriting, or transcription when an API key is configured.
- Provides ten read-only MCP tools backed by the authenticated local daemon.
- Presents a compact top-edge companion, a memory hub, and boxer artwork.

## Components

- `apps/woof`: Tauri desktop process and Svelte interface.
- `apps/woof_d`: loopback-only daemon for capture, persistence, search, memory, reminders, and time tracking.
- `apps/woof-mcp`: optional stdio MCP server.
- `crates`: shared Rust libraries for configuration, secure storage, capture, search, audio, and OpenAI access.
- `docs/contracts`: stable machine-readable HTTP, MCP, SQLite, and identity specifications.
- `assets`: canonical boxer and menu-bar artwork.
- `scripts`: supported build, audit, signing, and release commands.
- `vendor/wry`: pinned Wry 0.54.4 patch with documented provenance.

See [architecture](docs/architecture.md) for component responsibilities and trust boundaries.

## Requirements

- Apple silicon Mac running macOS 14 or later.
- Rust 1.88.0.
- Node.js and npm.
- Xcode Command Line Tools, including `codesign`, `security`, `swift`, and `sips`.
- ImageMagick at `/opt/homebrew/bin/magick` when regenerating icons.

## Develop

Run commands from the repository root:

```sh
npm ci
npm run dev
```

The root command builds and stages fresh debug sidecars before starting Tauri. Use `scripts/stage-sidecars.sh debug` only for a stage-only refresh.

Build the desktop application with staged sidecars:

```sh
npm run build
```

The first local desktop build creates an isolated, self-issued development signing identity and reuses it for later builds. This keeps macOS permission identities stable while developing. It is not a distributable release identity; `scripts/build-release.sh` uses the separate Developer ID and notarization path.

Build only the web interface with:

```sh
npm run build --workspace apps/woof
```

## Verify

Run the authoritative source suite:

```sh
scripts/verify.sh
```

Success ends with `woof source verification passed.` The suite covers Rust formatting, tests, and linting; Svelte and TypeScript checks; JavaScript tests; the production web build; contract parsing; and the source-level privacy, network, and runtime-boundary audits. See [verification](docs/verification.md) for focused commands and manual product checks.

## First run

1. Launch `woof.app`.
2. Grant Accessibility to woof. When prompted for the capture service, use the revealed `woof_d` file with Accessibility’s `+` button; macOS attributes an automatic child-process prompt to the parent app instead of creating the required helper entry.
3. Add an OpenAI API key in settings if remote memory generation, chat, rewriting, or transcription is needed.
4. Review the capture, retention, application-exclusion, reminder, and notification settings.

Capture can be paused or resumed at any time. Delete all clears stored activity and derived memory while preserving an empty version-18 database schema.

## MCP

The installed app includes a read-only stdio server. Keep woof running, then use the MCP configuration generated in settings or point a compatible client at the bundled executable:

```json
{
  "mcpServers": {
    "woof": {
      "command": "/Applications/woof.app/Contents/MacOS/woof-mcp"
    }
  }
}
```

The server exposes exactly ten tools defined in [`mcp-tools.json`](docs/contracts/backend/mcp-tools.json). It forwards requests to the local daemon with the bearer token and can't modify stored data.

## Local service and data

The daemon listens only on `127.0.0.1:3334`. `GET /health` is public; every other route requires the bearer token stored in `~/.woof/api-token`.

woof keeps configuration under `~/.woof` and application data under `~/Library/Application Support/woof`. Sensitive files use mode `0600`, and private directories use mode `0700`.

macOS Keychain stores the OpenAI API key under service `com.julius.woof.openai`, never in configuration files or logs. Local capture and search continue to work without a key. See [privacy](docs/privacy.md) and [security](docs/security.md) for the full data boundary.

## Release

Production releases require an Apple Developer ID Application certificate and an authenticated `notarytool` Keychain profile. Create the profile once:

```sh
xcrun notarytool store-credentials woof-production
```

Then check prerequisites and run the release pipeline:

```sh
scripts/build-release.sh --check-prerequisites
scripts/build-release.sh
```

The script builds from clean derived output, verifies source and runtime boundaries, signs and notarizes the application, staples the ticket, checks Gatekeeper acceptance, and writes the archive, source manifest, and checksum under `artifacts/release`. The self-issued signing helpers are for local development and can't produce a distributable artifact. See [release status](docs/release-status.md) for every automated and manual gate.

## Documentation

- [Architecture](docs/architecture.md)
- [Privacy](docs/privacy.md)
- [Security](docs/security.md)
- [Interface specifications](docs/contracts/README.md)
- [Verification](docs/verification.md)
- [Release status](docs/release-status.md)
- [Vector index format](crates/woof-search/VECTOR_FORMAT.md)
- [Third-party notices](THIRD_PARTY_NOTICES)
