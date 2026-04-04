# nostr-double-ratchet-nostr

Nostr adapter for `nostr-double-ratchet`.

This crate owns all Nostr-specific translation:

- direct-message events
- invite URLs
- invite events
- invite response events
- roster events

## Core Conversions

- `message_event(...)`
- `parse_message_event(...)`
- `invite_url(...)`
- `parse_invite_url(...)`
- `invite_unsigned_event(...)`
- `parse_invite_event(...)`
- `invite_response_event(...)`
- `parse_invite_response_event(...)`
- `roster_unsigned_event(...)`
- `parse_roster_event(...)`

It depends on the core crate and translates only between Nostr artifacts and core domain types.
The core crate itself stays free of Nostr event and URL concepts in its public API.
