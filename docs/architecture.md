# architecture

woof is a native macOS application with three executable components and shared Rust libraries.

## Desktop application

The Tauri process owns the menu-bar lifecycle, top-edge window, settings interface, Keychain access, and daemon supervision. Its static `LSUIElement` metadata and runtime Accessory activation policy keep the agent out of the Dock. It starts one daemon child, proves daemon ownership with a fresh challenge tied to the local bearer token, passes the persisted pause state before capture starts, and terminates the child during shutdown.

The Svelte interface communicates with trusted Tauri commands and the authenticated daemon API. Source-controlled window dimensions and activation behavior keep the compact and expanded states deterministic.

Inline activation retains the original Accessibility text target before showing an overlay. A non-empty selection opens selection rewriting, a non-empty editor opens whole-draft rewriting, and an empty message editor can start contextual reply drafting. Contextual reply mode is currently restricted to the exact WhatsApp Web HTTPS host and the exact Slack desktop bundle identity. It additionally requires a supported message-composer semantic label; WhatsApp must expose that composer beneath an `AXWebArea` carrying the same exact HTTPS host. Blank search, single-line text, combo-box, canvas, unlabeled, unsupported-app, and ambiguous editable targets remain ordinary rewrite targets or fail closed. For a contextual reply, the daemon performs one fresh foreground capture while the source application is still frontmost and compares the expected process and required window title from shallow metadata; when both clients expose a positive `AXWindowNumber`, it is compared as an additional best-effort signal. Exact Slack bundle or WhatsApp URL-only metadata is required before recursively reading text. After the tree read, the daemon requires the global focused window and focused element to remain the exact retained AX objects and rechecks title, browser metadata, and any available window number. It extracts a bounded recent-text region from the nearest viable composer container, ordered by visual position above the composer and tagged with left/center/right geometry hints. Consistent WhatsApp message alignment can provide authorship evidence; Slack requires visible sender labels that explicitly distinguish user-authored and incoming messages, otherwise generation fails closed as insufficient context. Selection and draft rewrites do not perform this fresh window capture. Only after visible-context capture succeeds does the desktop process validate the retained target again, open the edit overlay, and begin the LLM request. While that request is in flight, session-owned, revision-checked Accessibility writes cycle the exact whole-draft values `generating.`, `generating..`, and `generating...`; marker updates never use clipboard or keyboard fallback and stop if the exact edit controller, target content, or focus changes. A failure or cancellation restores the original empty revision only while the edit controller still owns keyboard focus and the retained element still contains woof's exact current marker. If either proof is unavailable, woof leaves the composer untouched and surfaces that temporary text may remain, so cleanup never races or clears a user edit. Whole-draft insertion then replaces the last confirmed marker, is reread, and must exactly match the generated result before woof reports success. The result remains in the retained composer for review; woof never invokes the application's send action.

Accessibility onboarding requires both native clients: the Tauri process checks its own TCC state for inline rewriting, while an authenticated daemon status check verifies the independently trusted, running `woof_d` capture process. macOS grants Accessibility per executable, but attributes a prompt from the bundled child daemon to the parent application instead of creating a distinct helper entry. The app therefore requests woof normally, then opens Accessibility and reveals the exact signed `woof_d` file for the user to add with `+`. Onboarding exposes each status separately and rechecks both after the daemon resume response before persisting completion.

## Daemon

`woof_d` owns Accessibility capture, persistence, local semantic search, generated memory, reminders, time tracking, and local HTTP routing. It binds exactly to `127.0.0.1:3334`. Only `GET /health` is public. Authentication runs before all other route matching so unknown and malformed protected requests cannot reveal route structure.

The authenticated contextual-reply capture route holds the same pause and blacklist policy boundary as background capture. It fails closed before a recursive text read on an unsupported surface, and later fails closed on a non-message or non-empty focused editor, secure input, missing or ambiguous geometry, a changed foreground process/window/element, or unavailable Accessibility data. It redacts and bounds the extracted text, returns it only to the requesting desktop process, zeroizes temporary unredacted extraction buffers, and does not write the one-shot route result or add the generated reply to local inline-rewrite examples. After insertion, the draft is ordinary visible application text and may be stored by background capture under the user's capture, exclusion, and retention settings.

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
