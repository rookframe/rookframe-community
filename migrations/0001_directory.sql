CREATE TABLE world_addresses (
    world_id uuid NOT NULL,
    world_address uuid NOT NULL,
    administrator_digest bytea NOT NULL,
    revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    listing jsonb,
    checked_in_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (world_id, world_address)
);
CREATE TABLE directory_operations (
    world_id uuid NOT NULL,
    world_address uuid NOT NULL,
    operation_id uuid NOT NULL,
    request jsonb NOT NULL,
    result jsonb NOT NULL,
    PRIMARY KEY (world_id, world_address, operation_id),
    FOREIGN KEY (world_id, world_address) REFERENCES world_addresses
);
CREATE INDEX directory_recent ON world_addresses (checked_in_at DESC)
    WHERE listing IS NOT NULL;
