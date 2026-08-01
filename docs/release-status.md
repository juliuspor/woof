# release status

Approve a release candidate only after the automated and manual gates below pass for the exact source state being shipped.

## Automated gates

Run:

    scripts/build-release.sh --check-prerequisites
    scripts/build-release.sh

Invoke the script directly as shown. Calling it through `sh` or another interpreter bypasses the kernel shebang and is rejected before any release prerequisite or build step runs.

The script must:

- require exactly one selected Apple Developer ID Application identity with a usable private key;
- start through a kernel-enforced empty environment, reconstruct only the fixed release environment, and require a preconfigured `notarytool` Keychain profile;
- install locked JavaScript dependencies;
- regenerate icons from the canonical boxer artwork;
- build fresh arm64 daemon and MCP sidecars;
- require embedded Info.plist metadata for the single-file helpers and prove that ordinary signing derives the stable identifiers `com.julius.woof.daemon` and `com.julius.woof.mcp` from it;
- prevent inherited shell, interpreter, Tauri, Vite, Rust, Cargo, SQLite, compiler, linker, Node, npm, dynamic-loader, Git, proxy, and credential settings from reaching the release process, and reject local `.env`, `.npmrc`, and `.cargo` overrides;
- run the complete source verification suite;
- remove prior derived application output before bundling;
- remove the complete prior Cargo target tree before compiling and embedding the freshly built interface;
- produce `target/aarch64-apple-darwin/release/bundle/macos/woof.app`;
- scan source paths and bytes against the encoded product-integrity set after fresh sidecars are staged;
- enforce an exact bundle path, type, mode, symlink, and hard-link allowlist;
- verify the application and all executable signatures, exact code identifiers and designated requirements, bound helper metadata, Apple certificate chain, certificate fingerprint, secure timestamp, hardened-runtime flag, arm64 architecture, entitlement allowlist, system-only dependencies, absence of runtime search paths, and absence of build-host paths;
- verify bundle identifier `com.julius.woof`, URL scheme `woof`, version, icon, exact third-party-notice bytes, agent mode, and single-instance metadata;
- remap workspace and build-account source prefixes before compilation, then reject source maps, dSYM directories, build-host paths, and other debug payloads;
- verify exact HTTP contracts, challenged daemon health, private file creation, and the pinned static and live runtime network boundary;
- submit a temporary zip with `notarytool`, require Apple status `Accepted`, staple the ticket to the app, validate the ticket, and require Gatekeeper acceptance;
- create two byte-identical archives from the same stapled bundle;
- scan both archives, extract one, and verify the full bundle allowlist, staple, signature, and Gatekeeper assessment again;
- write the archive, source manifest, and checksum record under `artifacts/release`.

## Manual gates

Before distributing a candidate:

1. Launch the archived application on a clean Apple silicon macOS account.
2. Verify that the signed GUI and signed `woof_d` receive stable, independent Accessibility grants used by onboarding. Check the OpenAI background-processing disclosure before saving a key.
3. Test capture pause and resume, application exclusions, finite retention, permanent deletion, reminders, and notifications-off-by-default behavior.
4. Exercise local search, recall, generated memory, chat, inline rewriting, Realtime transcription, time tracking, and the ten read-only MCP tools.
5. Inspect the boxer menu-bar artwork and compact/expanded window geometry on a standard and a notched display.
6. Run `node scripts/audit-runtime-boundary.mjs --live` against the installed candidate.
7. Run the explicit current-runtime roots through `node scripts/audit-zero-remnants.mjs tree ...`.
8. After the final authorized history rewrite and Git pruning, require `node scripts/audit-zero-remnants.mjs git . --require-pruned` to pass.
9. Inspect logs for bearer tokens, OpenAI keys, captured text, or audio; none may appear.
10. Record the tested macOS version and the generated checksum files alongside the candidate.

## Signing

Production releases use an Apple-issued Developer ID Application identity. If exactly one valid identity is available, the pipeline selects it automatically. If several are available, select the non-secret certificate name or SHA-1 fingerprint explicitly:

    scripts/build-release.sh --signing-identity "Developer ID Application: ORGANIZATION (TEAMID)"

The daemon and MCP helper are plain Mach-O executables, so each embeds an Info.plist in its `__TEXT,__info_plist` section. Signing must derive `com.julius.woof.daemon` and `com.julius.woof.mcp` from that metadata; the release verifier rejects a hash-derived identifier, unbound metadata, or a designated requirement that does not bind the exact identifier, Apple Developer ID chain, and selected Team ID.

Before the first production release, create a credential profile interactively in Keychain. Don't put the Apple ID password, app-specific password, or API private key in a script or environment variable:

    xcrun notarytool store-credentials woof-production

Use another non-secret profile name with `--notary-profile PROFILE`. `--check-prerequisites` verifies the local certificate, private-key access, secure timestamp, Apple distribution requirement, release tools, and authenticated notary-service access through the Keychain profile before dependency installation or compilation. A missing prerequisite stops the pipeline without creating an artifact.

`scripts/create-local-signing-certificate.sh`, `scripts/prepare-signing-keychain.sh`, and `scripts/sign-app.sh` are development-only helpers. The production pipeline never uses or notarizes their self-issued `woof local development signing` identity, and the prerequisite check rejects it.

Apple's current distribution requirements are described in [Notarizing macOS software before distribution](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution), [Create Developer ID certificates](https://developer.apple.com/help/account/certificates/create-developer-id-certificates/), and [Developer ID](https://developer.apple.com/developer-id/).
