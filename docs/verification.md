# verification

Verification is split into source checks, release checks, runtime-boundary checks, and manual product checks.

## Source suite

Run from the repository root:

    scripts/verify.sh

The suite performs:

- Rust formatting;
- locked workspace metadata resolution;
- Rust tests for every workspace target;
- Clippy with warnings denied;
- fresh debug sidecar compilation, staging, and code-identity checks;
- Svelte and TypeScript checks;
- JavaScript tests;
- the production web build;
- explicit scans of the generated web and SvelteKit output trees;
- JSON parsing for every machine-readable contract;
- adversarial tests for the encoded zero-remnant scanner;
- tracked and non-ignored untracked source path-and-byte scanning;
- a fail-closed inventory of production network clients and destinations;
- static runtime-boundary assertions.

The command must finish with `woof source verification passed.`

## Focused checks

Useful individual commands:

    cargo fmt --all --check
    cargo test --workspace --all-targets --locked
    cargo clippy --workspace --all-targets --locked -- -D warnings
    scripts/verify-code-identities.sh target/aarch64-apple-darwin/debug/woof_d target/aarch64-apple-darwin/debug/woof-mcp target/aarch64-apple-darwin/release/woof_d target/aarch64-apple-darwin/release/woof-mcp
    npm run check
    npm test
    npm run build --workspace apps/woof
    find docs/contracts -name '*.json' -type f -exec jq empty {} +
    node scripts/audit-zero-remnants.mjs self-test
    node scripts/audit-zero-remnants.mjs source .
    node scripts/audit-runtime-boundary.mjs

## Runtime boundary

With woof running from `/Applications/woof.app`:

    node scripts/audit-runtime-boundary.mjs --live

The live audit verifies:

- one desktop process and one supervised daemon with only the accepted arguments;
- exact loopback binding on `127.0.0.1:3334`;
- exact public health behavior, a fresh HMAC-SHA256 ownership proof, and authentication-before-routing;
- exact agreement between both HTTP contract ledgers and the daemon router;
- private permissions for configuration, token, database, WAL, and shared-memory files;
- configuration paths confined to woof state directories;
- either valid supervised daemon command form, including the optional persisted `--start-paused` argument;
- every descendant of the desktop, daemon, and active MCP processes across repeated samples;
- no UDP socket;
- only exact IPv4-loopback TCP flows involving port `3334`;
- no observed remote destination except a currently resolved `api.openai.com:443` address.

The installed application and its runtime state are read-only during this check.
The bounded, sampled observation supports release verification only; it doesn't enforce network traffic. Exercise chat and transcription during dedicated repeated runs when live remote acceptance is required.

## Zero-remnant modes

Use the standalone scanner for explicit roots:

    node scripts/audit-zero-remnants.mjs tree target/aarch64-apple-darwin/release/bundle/macos/woof.app
    node scripts/audit-zero-remnants.mjs tree EXTRACTED_ARCHIVE_ROOT
    node scripts/audit-zero-remnants.mjs tree ~/.woof "$HOME/Library/Application Support/woof"

Quit woof before auditing current runtime roots so the database and sidecars remain stable. Tree mode checks path bytes, regular-file bytes, and symlink target text without following symlinks. It refuses excluded read-only reference paths and ancestor roots that could include them.

The Git audit is read-only. After an authorized history rewrite, and only after preserving any backup that is still needed, expire reflogs and prune unreachable objects before requiring the final state:

    git reflog expire --expire=now --all
    git gc --prune=now
    node scripts/audit-zero-remnants.mjs git . --require-pruned

Git mode scans ref names and values, reflog and metadata bytes, and all reachable and unreachable objects returned by Git. `--require-pruned` also rejects non-empty reflogs, alternates, and any object Git still classifies as unreachable.

## Release

Run:

    scripts/build-release.sh --check-prerequisites
    scripts/build-release.sh

The production command accepts `--signing-identity IDENTITY` when more than one Apple Developer ID Application certificate is available and `--notary-profile PROFILE` when the Keychain profile is not named `woof-production`. Identity and profile names are non-secret selectors. Store notarization credentials interactively with `xcrun notarytool store-credentials PROFILE`; never pass credentials through environment variables or release logs.

Keep the generated `.zip`, `.zip.sha256`, and `.zip.sources` files together. Verify the checksum record with:

    cd artifacts/release
    shasum -a 256 -c NAME.zip.sha256

The helper identity command reads each embedded Info.plist, signs only private temporary copies without an explicit identifier, and requires both builds to resolve to the same exact daemon and MCP identities with bound metadata. It leaves the source binaries unchanged and can't substitute for an Apple Developer ID or TCC acceptance test.

The release pipeline remaps workspace and build-account source prefixes, removes the complete prior Cargo target tree before the final application compile, and rejects any surviving build-host path. It then validates thin arm64 executables, exact stable code identifiers and designated requirements, bound helper metadata, an Apple Developer ID Application chain and exact leaf fingerprint, hardened-runtime signatures, secure timestamps, the entitlement allowlist, system-only dynamic dependencies, absence of runtime search paths, exact bundle paths and modes, no symlinks or debug payloads, exact metadata, icon bytes, and third-party-notice bytes, source stability, sidecar stability, and source/bundle/archive identity scans. It submits a temporary zip with `notarytool`, requires Apple status `Accepted`, staples and validates the app ticket, requires Gatekeeper acceptance, and packages only that stapled app. It creates two byte-identical final archives for the same normalized stapled bundle and repeats staple, signature, Gatekeeper, layout, and content validation after extraction.

The local self-issued signing scripts are development-only and are not part of this workflow. A missing Developer ID identity, secure timestamp, Keychain profile, accepted notarization response, staple, or Gatekeeper assessment stops the production command before publishing an artifact.

## Manual product checks

On a clean test account:

1. Launch woof with Accessibility denied and verify that onboarding remains blocked. Request access from onboarding, grant both signed woof entries, and proceed only after the GUI is trusted and the daemon-owned capture check reports trusted and running. Revoke either grant before finishing; capture must remain paused and onboarding must return to Accessibility. This TCC interaction requires a real signed app. Automated tests alone can't prove it.
2. Open, expand, collapse, and dismiss the compact top-edge window.
3. Test capture status, pause, resume, persisted-paused startup, secure-input refusal, and excluded-application settings.
4. Open recent activity, local semantic search, wiki, generated memory, and time views with stored data.
5. Before saving an OpenAI key, verify that onboarding discloses periodic background memory generation and the data classes it may send.
6. Add and remove an OpenAI key, then exercise chat, inline rewriting, and Realtime transcription without exposing the key.
7. Start with a fresh profile and verify that local nudges are off. Opt in, create and delete reminders, and check due delivery while woof is running. Quit woof before another reminder becomes due; woof must surface it after the next launch without claiming delivery as an OS-scheduled alarm while the app was closed.
8. Select a finite retention window and verify that woof removes expired source, derived memory, and any database quarantine copies immediately and after restart.
9. Run delete-all and check that it leaves an empty, usable database at schema user version 18 while preserving preferences and the Keychain item.
10. Exercise all ten read-only MCP tools and reject an unknown tool, unknown argument, oversized frame, and invalid health proof.
11. With a disposable test database, exercise successful quarantine recovery. The GUI must show a dismissible fresh-storage notice, and neither its payload nor logs may contain paths or captured content.
12. Quit woof and verify that the daemon exits.

Record failures with the commit identifier, macOS version, command, and redacted output.
