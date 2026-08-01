# backend specifications

The daemon and MCP server consume or are tested against the JSON files in this directory.

## HTTP

`http-routes.json` fixes the loopback listener at `127.0.0.1:3334`, makes only `GET /health` public, defines its optional HMAC-SHA256 ownership proof, requires constant-time bearer authentication before every other route, and lists every retained method/path pair. Source verification rejects a missing, extra, or duplicate route.

The reminder routes store and materialize local rules only while the daemon is running; they don't promise an OS-scheduled alarm. The work-pattern routes expose detection and review state only. An `accepted` workflow row records a local pattern for review and carries no executable behavior.

## MCP

`mcp-tools.json` defines exactly ten read-only tools. Each maps only to a GET route in the HTTP contract. The Rust MCP server compiles this file into its binary, so names and input schemas are part of the public interface.

## SQLite

`sqlite-v18.json` records the version-18 storage layout and exact named structures. The unusual index spelling and trigger set are intentional persisted properties and must remain stable.
