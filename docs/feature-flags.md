# Feature Flags

All feature flags in OpenAB, their current defaults, and planned changes.

At each major version release, review this table and flip flags marked for that version. Update the CHANGELOG with each flip as a BREAKING change.

Tracking issue: [#510](https://github.com/openabdev/openab/issues/510)

## Flags

| Flag | Config Path | Current Default | Planned Default | Target Version | Context |
|------|------------|----------------|----------------|---------------|---------|
| `per_thread_workdir` | `[pool]` | `false` | `true` | v1.0.0 | Per-thread isolated working directories (PR #41) |

## Adding a New Flag

1. Add a row to the table above
2. Add a checkbox to the [tracking issue](https://github.com/openabdev/openab/issues/510)
3. Add a code comment: `// TODO(v1.0): flip default to <value>`
4. Document the flag in the config reference

## Release Checklist

When cutting a major version:

1. Filter this table by `Target Version`
2. Flip each default in code
3. Update this table (`Current Default` ← `Planned Default`, clear `Target Version`)
4. Add each flip to CHANGELOG as BREAKING
5. Document in migration guide
6. If a flag's new default is the only sensible behavior, remove the flag entirely and keep the behavior (no dead config)
