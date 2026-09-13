# Community Server API v1

The API prefix is `/api/v1`. JSON uses snake_case. HTTPS is required outside
loopback development. This release provides Directory publication/discovery;
Join Requests, Invitations, admission, WebRTC setup and TURN are unavailable.
The service neither stores Worlds nor authenticates Participants.

`GET /health` checks PostgreSQL and migration readiness. `GET /live` checks the
process. `GET /worlds?q=words&offset=0&limit=30` returns `listings` and optional
`next_offset`. `GET /worlds/{world_id}/{world_address}` returns one listing or
404. Each entry contains both UUIDv4 identifiers, `revision`, and `listing`.
Duplicate display names are allowed. Unchecked listings disappear after 30 days.

`PUT /worlds/{world_id}/{world_address}` requires `Authorization: Bearer` with
an independently generated 32-byte random token encoded as 64 hexadecimal
characters. The first committed publication reserves this unpredictable World
Address for that administrator capability. The database retains only its SHA-256
digest and never releases the reservation on removal. This is a per-address
capability, not a user account; keep it private and back it up separately from
public World data. A caller-supplied World ID alone grants no access.

The body contains `operation_id` (UUIDv4), `expected_revision` (initially zero),
and `listing` (object for publication, null for removal). Required listing fields:
`name` (1–100 UTF-16 units), `description` (1–4000), `game_system` (1–200),
`language` (1–100), `player_limit` (integer 1–1000). Optional `schedule` is at most
500 units; optional `cover_image` is an HTTPS URL at most 2048 units. The server
never fetches covers. Inputs must be trimmed and may not contain control characters.

The transaction authenticates ownership, serializes changes to this address,
checks the expected revision, and commits the listing/removal together with a
stable operation result. A successful result contains `operation_id`, `revision`,
and `visibility` (`public` or `private`). Repeating the identical operation with
the same credential returns the original result, even after later changes;
changed input for a retained operation returns 409. Delayed retries cannot
republish removed listings. Wrong ownership returns 403; invalid input 400;
missing records 404; conflicting revisions 409; storage unavailability 503.

Clients durably retain the operation, capability, and World candidate before
sending. A lost reply is uncertain, never a confirmed rejection: retry the same
operation. Report final success only after the HTTP result and the coherent local
World commit. Retain a failed local commit for retry without issuing a new remote
operation. New Private Worlds do not need this service.
