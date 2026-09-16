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
`next_offset`. Search includes name, description, game system, language and schedule.
Optional `system` and `language` filters match exact values case-insensitively;
`online=true|false` and `full=true|false` filter availability and capacity.
Entries include `online` and `checked_in_at`. Order is Online accepting, Offline
accepting, then Full; each group uses recent authenticated check-in, followed by
stable identity/address ties.

`GET /worlds/{world_id}/{world_address}` returns one listing or
404. Each entry contains both UUIDv4 identifiers, `revision`, and `listing`.
Duplicate display names are allowed. Unchecked listings disappear after exactly
30 days. Anonymous reads do not extend that deadline. Authenticated setup
startup/renewal checks in the same address, so returning revives its existing
listing without a metadata mutation. Online is derived from the existing 30-second
setup lease; stop revokes it, and crashes/unclean shutdowns expire it. Restarting
the service drops volatile availability while keeping Offline listings.

`GET /worlds/{world_id}/{world_address}/administration` authenticates the same
administrator Bearer capability and returns `{revision}` including after removal
or expiry. An address never reserved on this server returns revision 0. Manager
uses this to move a listing to a configured replacement server (or back again)
without depending on the old server. The replacement publication remains an
ordinary revision-checked idempotent mutation. Local deletion/server replacement
may abandon an old listing, which ages out without further check-in; its requests
follow normal bounded retention.

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


## Private Join Requests

`/api/v1/worlds/{world}/{address}/requests/{request}` supports applicant `PUT`
(submit `{name,message}`) and `GET` (status). `/withdraw` accepts applicant `POST`.
The applicant generates one UUID v4 per logical request and retains a 64-character
hex bearer proof before sending. Repeating the same ID and body recovers the same
submission. Names are required (1–100 UTF-16 units); private messages are required
(1–4,000). Duplicate names are allowed. A World/installation can hold only one
pending or accepted, non-removed request. Pending requests reserve no capacity.

The World administrator bearer can `GET .../requests` (at most 200 records,
pending first), and `PUT .../{request}/decision` with a UUID `decision_id` and
`action`: `prepare`, `abort`, `accept`, `reject`, or `reject-block`. Rejection accepts an optional
private `response` (1–4,000). Acceptance requires the receipt authored by the World
(`seat_id`, exact `name`, `credential`); the Seat ID equals the request ID.

Manager journals the acceptance intent privately, prepares the service decision,
commits the Seat through stopped World administration or the running Authority,
then acknowledges that exact receipt and synchronizes capacity. Prepare fences
withdrawal and other decisions. A known pre-commit failure aborts preparation;
a lost reply or uncertain World outcome retains the journal for reconciliation.
The service never authors a Player identity or reserves a Seat. Retries of a
completed decision return the same outcome. Applicants alone receive credentials;
GM review receives an installation hash, never the applicant's bearer proof.

Undecided requests expire after 30 days. Recruitment name/message text is scrubbed
at that deadline, including interrupted decisions. A prepared decision awaiting
its World outcome retains only reconciliation metadata until Manager resolves it;
expiring it blindly could contradict an already committed Seat. Terminal status,
response, and accepted receipt are removed 30 days after the terminal outcome.
Maintenance runs every minute and before request reads/mutations. All request
responses use `Cache-Control: no-store`; request bodies are not logged or public.

Limits: 100 pending requests per listing; hourly submissions 10 per installation,
100 per network address; status/withdrawal checks 600 per installation, 6,000 per
network address. PostgreSQL counters and listing row locks enforce these limits
under concurrency. Caddy overwrites `X-Rookframe-Client-IP`; only the private
container listener with `TRUST_PROXY=true` trusts that header. Standalone servers
use the TCP peer address.

## Player removal, installation blocks and abuse reports

All paths below start with `/api/v1`. They use the existing bearer authorization,
strict body bounds and `Cache-Control: no-store`. Bodies and headers must never
be logged. A World administration capability is not a gameplay credential.

- `GET /worlds/{world}/{address}/blocks` requires that World's administrator. It
  returns at most 1,000 private `{installation_hash,name,created_at}` records.
  `DELETE .../blocks/{installation_hash}` unblocks new requests idempotently.
- `reject-block` atomically rejects a pending request and blocks its installation.
  Ordinary `reject` never creates a block. Rejection retries must use the same
  decision ID, response and block choice. Accountless blocks can be evaded by a
  new installation. Existing accepted Players must be removed separately.
- `PUT /worlds/{world}/{address}/players/{seat}/removal` requires administration
  and `{operation_id,installation_hashes,name,block}`. Manager has already removed
  the Seat and revoked all Sessions durably. Up to 33 installation hashes can be
  blocked. The service scrubs that Seat's accepted receipt and marks its request
  `removed:true`; it never mutates a World. Removed/rejected applicants may submit
  a new request unless blocked. An exact removal retry does not recreate a block
  that was subsequently lifted. Manager retains its private recovery journal
  until cleanup and capacity projection are confirmed.
- `PUT /worlds/{world}/{address}/reports/{report}` accepts a stable UUID and
  `{request_id:null,reason}` for a currently public listing. A request report uses
  its request UUID and requires World administration. Reason is required,
  1–1,000 UTF-16 units. A retry must match the original target, author and body.
  Private recruitment messages and World credentials are not copied into reports.
- `GET /operator/reports` returns the oldest 100 unresolved reports.
  `PUT /operator/worlds/{world}/{address}` with `{hidden:true}` hides discovery and
  new requests; `{hidden:false}` restores eligibility. Both require the separate
  operator bearer configured by `MODERATION_TOKEN_SHA256`. GM credentials cannot
  call them. The action resolves this address's reports and changes only service
  presence. It cannot delete or modify a local World, its Actors or Sessions.

Reports and removal-operation receipts expire after 30 days. Installation blocks
remain until explicitly removed, bounded at 1,000 per World Address. Existing
Join Request response/text retention remains unchanged. Removal deletes the
accepted credential immediately without deleting the private response early.
Submission limits (10/installation/hour, 100/network/hour) cover reports and
unblocking. Read/decision/removal/operator limits are 600/installation/hour and
6,000/network/hour; these share the request counters and cannot evade them by
switching endpoints. New requests also reject `world_full` or
`installation_blocked` without reserving a Seat.

For an absent request, authenticated `GET /worlds/{world}/{address}/requests/{id}`
returns `installation_blocked` when that same installation is blocked for the
World Address. Existing retained requests remain readable. This lets a client
recover the block without a submission attempt when its write quota is exhausted.
The ordinary private-read installation/network limits still apply.

Expired prepared requests report Expired and scrub recruitment text at 30 days.
Their original decision proof remains for the additional 30-day terminal window,
so an already committed World receipt can finish reconciliation. Unreconciled
orphans are then deleted; deleted local Worlds need not retain request journals.
