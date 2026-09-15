CREATE TABLE join_requests (
    request_id uuid PRIMARY KEY,
    world_id uuid NOT NULL,
    world_address uuid NOT NULL,
    installation_digest bytea NOT NULL,
    submission_digest bytea NOT NULL,
    name text,
    message text,
    status text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','accepted','rejected','withdrawn','expired')),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL DEFAULT now() + interval '30 days',
    terminal_at timestamptz,
    response text,
    receipt jsonb,
    decision_id uuid,
    FOREIGN KEY (world_id,world_address) REFERENCES world_addresses,
    CHECK ((status = 'pending') = (terminal_at IS NULL))
);
CREATE UNIQUE INDEX join_request_active ON join_requests (world_id,world_address,installation_digest)
    WHERE status IN ('pending','accepted');
CREATE INDEX join_request_review ON join_requests (world_id,world_address,created_at);
CREATE TABLE request_rate_limits (
    scope text NOT NULL,
    identity_digest bytea NOT NULL,
    bucket timestamptz NOT NULL,
    hits integer NOT NULL,
    PRIMARY KEY (scope,identity_digest,bucket)
);
