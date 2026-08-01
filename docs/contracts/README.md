# woof interface specifications

These machine-readable files define the stable external surfaces and local invariants of woof:

- `identity.json`: product, application, daemon, and MCP identifiers and names, listener, Keychain service, and build architecture.
- `http.json`: exact public health and ownership-proof behavior, authentication policy, and complete authenticated route ledger.
- `backend/http-routes.json`: detailed daemon route groups and request limits.
- `backend/mcp-tools.json`: the ten MCP tool definitions compiled into `woof-mcp`.
- `backend/sqlite-v18.json`: authoritative SQLite user version, tables, FTS structures, triggers, indexes, and preserved schema properties.

The source verification audit derives the method/path pairs from the daemon router and requires both HTTP ledgers to match exactly. Changes to these files must ship with matching implementation and test updates. Sensitive example values do not belong in specifications.
