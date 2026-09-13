# Community Server API v1

The API prefix is `/api/v1`. JSON uses snake_case. HTTPS is required outside
loopback development. This release provides Directory publication/discovery and direct WebRTC setup;
Join Requests, Invitations, admission and TURN are unavailable.
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
never fetches covers. Inputs must be trimmed. Only `description` permits CR/LF line
breaks; other control characters are rejected, and all other fields reject every
control character. The description's 4000-unit bound includes its line breaks.

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

## Direct WebRTC setup (RFG-235)

The same HTTPS origin exposes `/api/v1/setup/worlds/{world_id}/{world_address}`.
`PUT` with the existing administrator Bearer capability and `{ "locator": UUIDv4 }`
reserves/renews a 30-second availability lease. Renew every 10 seconds. The locator
names this running World lifetime; a different live locator returns 409. Private
startup reserves address ownership without publishing a listing or changing its
revision. `GET` returns the exact identities and locator, or 404 when unavailable.
`DELETE ?locator=...` authenticates the administrator, removes that lifetime and
all pending attempts, and cannot revoke a different lifetime.

An installation chooses a random UUIDv4 attempt, a random peer ID in 2..2147483647,
and two independent cryptographic 32-byte hexadecimal secrets: its temporary
Bearer credential and a control proof. It creates native WebRTC channels, then
`PUT .../connections/{attempt}` with `locator`, `peer_id`, `sdp` (the offer),
`candidates` and `proof`. Each candidate has `mid`, `index` and `candidate`.
Exact retries return the same attempt; SDP, proof and peer ID cannot change.
Candidates accumulate idempotently. The response has `attempt_id`, `peer_id`,
nullable answer `sdp` and answer `candidates`. Client-authenticated `GET` reads the
same result. No HTTP redirect is required or followed by the application.

The administrator polls `GET .../connections?locator=...` for at most eight
pending offers (`connections`: attempt ID, peer ID, SDP, candidates, control
proof). It supplies its answer using `PUT .../connections/{attempt}/answer`
with locator, SDP and candidates. Only the administrator may answer, list the
private offer queue or renew/revoke availability. Each client may read/update
only its own attempt. Client peer IDs never include Authority peer 1 and have
no Participant meaning.

After its encrypted control handshake completes, the client deletes the attempt
with `DELETE .../connections/{attempt}`. Either its credential or the World
administrator can delete it. Clients also delete on cancel/failure. Attempts have
an absolute 60-second expiry, including while availability renews; reads and
candidate retries never extend it. Availability and all SDP, ICE and proof data
are volatile, removed on expiry or process restart, and swept every five seconds
without waiting for a request. Established WebRTC connections do not use this
service and survive its loss.

Bounds: 32 KiB request body; 16 KiB SDP; 32 distinct candidates per side (each
candidate at most 2048 bytes and mid 64 bytes); eight pending attempts and sixteen
new attempts/minute per World lifetime; 150 client operations/attempt; 512 live
World leases; 200 setup operations/second globally; the existing 64 concurrent
requests and 15-second HTTP deadlines also apply. Excess load returns 429/503.
Missing/expired World or attempt returns 404; wrong credentials 401/403; conflicting
locator or replay 409. Bodies, credentials, proofs and SDP are never logged.

The Rookframe control endpoint uses native SceneMultiplayer authentication data;
transport authentication does not call `complete_auth`, load client Packages,
create a World Session or enable gameplay RPCs. No Invitation, Join Request,
Participant admission, TURN allocation or relay attempt is implemented here.
