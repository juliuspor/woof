# privacy

woof keeps its primary activity store on the user's Mac and makes the optional remote-processing boundary explicit.

## Data stored locally

After the user grants Accessibility permission, woof may store:

- foreground application and window metadata;
- captured on-screen text and focused-element context;
- activity timing and derived summaries;
- searchable indexes, wiki pages, and time-classification rules;
- reminders, notification state, settings, permission state, and local conversation state.

woof stores the SQLite database at `~/Library/Application Support/woof/woof.db` and keeps configuration and the bearer token under `~/.woof`. Private directories use mode `0700`, and sensitive files use mode `0600`.

## OpenAI processing

OpenAI access is optional. Saving an OpenAI API key enables automatic periodic memory generation. While the user keeps a key configured, woof may send bounded, locally redacted captured text; application, window, domain, and timing metadata; the user's messages to woof; existing generated summaries or wiki evidence; known project names; and unmatched time segments to `api.openai.com`. These background requests generate chronicles, wiki pages, actionable flags, and time-classification rules even when the user isn't actively using chat.

User-invoked chat and inline rewriting send the request and the bounded context needed to answer it. Realtime transcription sends microphone audio while dictation is active. Local Accessibility capture, full-text search, semantic embeddings, and vector search run without a remote embedding service.

woof doesn't use analytics, advertising endpoints, or third-party telemetry.

## Credentials

woof stores the OpenAI API key in macOS Keychain using service `com.julius.woof.openai` and generates the local daemon bearer token on device. Neither credential may appear in logs.

## Capture and notification controls

The user can pause and resume capture and exclude applications. Capture uses Accessibility text, not screenshots, and refuses capture during secure input. Removing the OpenAI API key prevents later operations from starting with that key; a scheduled generation run or user operation already in progress may finish.

Local nudges and macOS notifications are off by default. They require an explicit user opt-in. Users create and delete scheduled reminders only through settings. The daemon evaluates reminder schedules locally while woof is running and doesn't register future OS alarms. After the next launch, it evaluates reminders that became due while woof was closed. Open at login can keep this local scheduler available after sign-in.

Workflow detection only identifies recurring patterns in local memory. Keeping a detected pattern doesn't execute an action, control another application, or start automation.

## Retention and permanent deletion

The default retention policy keeps local memory until the user changes it. Settings offer finite retention windows. Selecting a shorter window immediately deletes expired captures, activity, chat, time records, and other aged source rows. Because generated memory can combine several sources, woof also invalidates derived summaries, wiki memory, detected follow-ups, and generated time rules when an expired source could survive there. A quarantined database can't prove row-level ages safely, so applying any finite retention window securely removes every quarantine copy before reporting success. The daemon enforces retention at startup and periodically while it runs.

Delete all permanently clears every logical SQLite data table with secure deletion enabled, checkpoints the WAL, vacuums the database, rebuilds empty full-text and semantic indexes, and resets local identity. It preserves the version-18 schema so the database remains usable. The OpenAI Keychain item and application preferences remain configured unless the user removes them separately.

Deleting the application doesn't automatically remove local data. To remove all woof state after quitting the application, delete `~/.woof`, `~/Library/Application Support/woof`, and the Keychain item for service `com.julius.woof.openai`.

## Logs

Operational logs may describe component startup, shutdown, and redacted failure categories. They must never contain bearer tokens, OpenAI keys, captured text, or audio.
