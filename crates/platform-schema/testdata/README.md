# Microsoft Teams manifest schema fixture

`MicrosoftTeams.v1.25.schema.json` is the Microsoft-published Teams app
manifest schema used for offline conformance tests.

- Source: <https://developer.microsoft.com/en-us/json-schemas/teams/v1.25/MicrosoftTeams.schema.json>
- Retrieved: 2026-08-13
- Provenance reverified: 2026-08-20
- Published-byte SHA-256: `24c1bbb38fc24ba19d536016fdfb6e8aced645ce9b3d0e19b4c0308ff47f5d96`
- Repository fixture SHA-256 after CRLF→LF normalization:
  `393557f89b0afc5b1134794d3af64c47a56ac29d44fdf977136c8121096f1503`

Only line endings are normalized; the JSON data is unchanged. Runtime code does
not load this file.

## Refresh Procedure

- **Trigger:** Microsoft changes the published v1.25 schema, the manifest
  conformance test changes schema version, or a deliberate fixture refresh is
  proposed.
- **Action:** fetch to a temporary path, compare both published and normalized
  hashes, review the JSON diff, then replace the fixture only in the same
  reviewed change:

  ```bash
  curl -fsSL \
    https://developer.microsoft.com/en-us/json-schemas/teams/v1.25/MicrosoftTeams.schema.json \
    -o /tmp/MicrosoftTeams.v1.25.schema.json
  python3 - <<'PY'
  from hashlib import sha256
  from pathlib import Path

  published = Path("/tmp/MicrosoftTeams.v1.25.schema.json").read_bytes()
  normalized = published.replace(b"\r\n", b"\n")
  fixture = Path("crates/platform-schema/testdata/MicrosoftTeams.v1.25.schema.json")
  print("published:", sha256(published).hexdigest())
  print("normalized:", sha256(normalized).hexdigest())
  print("matches fixture:", normalized == fixture.read_bytes())
  PY
  cargo test --manifest-path crates/platform-schema/Cargo.toml
  ```

- **Why:** the offline fixture is evidence for one exact Microsoft schema, not
  a generated approximation.
