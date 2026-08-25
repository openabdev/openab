# Cross-crate fixtures

`openab-agent` is its own Cargo workspace (excluded from the root one), so the
producer and the consumer of the turn envelope cannot share a test module. These
files are the shared truth instead: both sides `include_str!` the same bytes.

| Fixture | Produced by | Consumed by |
|---|---|---|
| `turn-envelope-v1.json` | `openab-agent/src/turn_envelope.rs` (`render`) | `crates/openab-core/src/structured_delivery.rs` (`parse_structured`) |
| `sequential-message-v1.json` | `openab-agent/src/acp.rs` (`AcpBubbleSink::emit`) | `crates/openab-core/src/acp/protocol.rs` (`classify_notification`) |

Changing a fixture breaks whichever side no longer agrees with it — which is the
point. See [ADR: Structured Delivery](../adr/structured-delivery.md).
