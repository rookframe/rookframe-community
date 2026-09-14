# Community Server API v1

The API prefix is `/api/v1`. JSON uses snake_case. HTTPS is required outside
loopback development. This release provides Directory publication/discovery,
WebRTC signaling and provider-neutral UDP TURN credentials. World Authority
Invitation claims and Player authentication use the encrypted application channel.
The service neither stores Worlds nor authenticates Participants.

`GET /health` checks PostgreSQL, migration readiness and configured TURN issuance
state (503 after an observed issuance failure until a successful issuance). It
does not probe the provider or prove that allocations work. `GET /metrics`
(at the origin root, outside the API prefix) exposes
aggregate issuance/revocation counters, with no credential or principal labels.
`GET /live` checks the
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

## WebRTC setup (RFG-235, RFG-243)

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
Bearer credential and a control proof. First `POST .../connections/{attempt}` with
`locator`, `peer_id` and `proof` reserves the attempt and returns
`{ ice_servers: [{ urls: [...], username?, credential? }], expires_at, principal }`.
The principal is an opaque UUIDv4 for provider attribution, not Player identity.
Responses are `Cache-Control: no-store`. ICE contains direct discovery plus UDP
TURN URLs; it never contains a backend provider token or shared secret. Initialize
stock WebRTC using this configuration before creating the offer, then
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

Before creating its native connection, the administrator sends
`POST .../connections/{attempt}/ice` with `{ locator }` and its administration
Bearer credential. This returns a separately attributed ICE configuration. Each
side receives the same retained issuance result on retry; cancellation does not
mint extra credentials. Provider calls have a five-second deadline, at most four
in flight and no automatic provider retries. A new attempt is required after a
failed issuance. Credentials last six hours by default (operator range 300–86400
seconds); credential expiry does not extend the 60-second setup lifetime.

After its encrypted control handshake completes, the client deletes the attempt
with `DELETE .../connections/{attempt}`. Either its credential or the World
administrator can delete it. Clients also delete on cancel/failure. Attempts have
an absolute 60-second expiry, including while availability renews; reads and
candidate retries never extend it. Availability and all SDP, ICE and proof data
are volatile, removed on expiry or process restart, and swept every five seconds
without waiting for a request. Established WebRTC connections do not use this
service and survive its loss.

Signaling deletion does **not** revoke the relay supporting established gameplay.
On cancel/disconnect, `DELETE .../connections/{attempt}/relay` separately denies
further issuance and attempts provider revocation. The client can revoke its own
grant; the administrator can revoke both sides. The reply contains
`issuance_revoked` and `provider_revoked`. Cloudflare revokes issued usernames;
coturn shared-secret credentials have no individual revocation API and return
`provider_revoked: false`: they remain valid until expiry. See emergency operator
revocation and session-duration constraints in the operations guide. Failed
Cloudflare revocation returns 503 and may be retried. Restart loses the volatile
revocation book; provider credentials remain bounded by their original expiry.

Bounds: 32 KiB request body; 16 KiB SDP; 32 distinct candidates per side (each
candidate at most 2048 bytes and mid 64 bytes); eight pending attempts and sixteen
new attempts/minute per World lifetime; 150 client operations/attempt; 512 live
World leases; 200 setup operations/second globally; the existing 64 concurrent
requests and 15-second HTTP deadlines also apply. Excess load returns 429/503.
TURN additionally allows 64 issuances/minute globally and retains at most 128
grants per World and 1024 globally until expiry, including revoked or failed
grants. Both sides count separately. Provider bandwidth/allocation quotas are
also required: HTTP issuance bounds do not bound relay bytes.
Missing/expired World returns `404 world_unavailable`; a missing/expired attempt
on a live World returns `404 attempt_unavailable`. Wrong credentials 401/403; conflicting
locator or replay 409. Bodies, credentials, proofs and SDP are never logged.
`503 relay_unavailable` means issuance failed; it does not diagnose a client's ICE
path. `403 relay_credential_revoked` is an evidenced revoked grant. Clients reject
configuration that is expired or cannot cover their remaining setup deadline.

The Rookframe control endpoint uses native SceneMultiplayer authentication data;
transport authentication alone does not call `complete_auth`, load client Packages,
create a World Session or enable gameplay RPCs. Invitation claim receipts and safe
Package requirements travel between applications, never through this service.
The existing World Authority Player authentication and bootstrap gate then admits
prepared Players identically for direct and relayed connections. TURN/TCP and
TURN/TLS are not supported.

## Invitation capacity (RFG-236)

Directory entries additionally contain `reserved_seats`, `claimed_seats` and
`full`. Reservations count all pre-created Player Seats, including claimed Seats,
and exclude the GM. Fully reserved listings remain discoverable as Full, after
accepting listings in browse order.

Publication accepts `capacity: { revision, reserved, claimed }`; omitted capacity
means the original zero-Seat state. Admission revision is nonnegative and
monotonic, with identical counts required for an equal revision. Counts obey
`0 <= claimed <= reserved <= 1000`. Public publication refuses a Player Limit
below reserved Seats or at/below claimed Seats.

`PUT /worlds/{world_id}/{world_address}/capacity` authenticates the same
administrator capability and accepts `expected_directory_revision`, `capacity`
and `remove_listing`. It serializes on the address, rejects stale revisions (409),
updates counts, and removes the listing when requested or fully claimed. It never
creates a listing or increments the Directory revision. A delayed cleanup cannot
remove an explicitly republished newer Directory revision. A repeated old publish
operation returns its receipt without recreating a removed listing.

The running World first commits a claim locally, including automatic Private
visibility on the final claim, then asynchronously retries this projection every
ten seconds. A failed service update does not undo the Seat or block preparation.
The service receives counts only: no Invitation secrets, installation keys, Seat
credentials, Player names, World saves or Package archives.
