# woof local Wry patch

This directory is the complete crates.io `wry` 0.54.4 package used by the
lockfile. The source archive SHA-256 is
`e5a8135d8676225e5744de000d4dff5a082501bf7db6a1c1495034f8c314edbc`, and
the packaged upstream revision is
`0b1e2befc0e813c28a2b1094170cfd8f185875d2`. Upstream Apache-2.0 and MIT
license files remain unchanged.

woof intentionally omits the upstream `.cargo/config.toml` because the release
preflight rejects nested Cargo configuration overrides. Cargo doesn't load
configuration from a dependency's directory, and the pinned source
archive above preserves the omitted 132-byte file for exact recovery.

woof audits dependency-generated JavaScript as part of every release. The two
platform copies of the synthetic mouse-event initializer therefore express two
standard DOM keys with computed-property syntax. This produces the same key
names and values in JavaScript and preserves event propagation and browser
behavior. The patch changes no other upstream behavior.

The narrow source edits are in:

- `src/webkitgtk/synthetic_mouse_events.rs`
- `src/wkwebview/synthetic_mouse_events.rs`
