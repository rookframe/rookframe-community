ALTER TABLE join_requests ADD COLUMN removed boolean NOT NULL DEFAULT false;
ALTER TABLE join_requests ADD COLUMN blocked boolean NOT NULL DEFAULT false;
DROP INDEX join_request_active;
CREATE UNIQUE INDEX join_request_active ON join_requests(world_id,world_address,installation_digest)
    WHERE status IN ('pending','accepted') AND NOT removed;
CREATE TABLE installation_blocks (
    world_id uuid NOT NULL,
    world_address uuid NOT NULL,
    installation_digest bytea NOT NULL,
    name text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(world_id,world_address,installation_digest),
    FOREIGN KEY(world_id,world_address) REFERENCES world_addresses
);
CREATE TABLE player_removals (
    world_id uuid NOT NULL,
    world_address uuid NOT NULL,
    operation_id uuid NOT NULL,
    payload_digest bytea NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY(world_id,world_address,operation_id),
    FOREIGN KEY(world_id,world_address) REFERENCES world_addresses
);
ALTER TABLE world_addresses ADD COLUMN moderated boolean NOT NULL DEFAULT false;
CREATE TABLE abuse_reports (
    report_id uuid PRIMARY KEY,
    world_id uuid NOT NULL,
    world_address uuid NOT NULL,
    request_id uuid,
    reporter_digest bytea NOT NULL,
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    resolved boolean NOT NULL DEFAULT false,
    FOREIGN KEY(world_id,world_address) REFERENCES world_addresses
);
