ALTER TABLE world_addresses ADD COLUMN admission_revision bigint NOT NULL DEFAULT 0 CHECK (admission_revision >= 0);
ALTER TABLE world_addresses ADD COLUMN reserved_seats integer NOT NULL DEFAULT 0 CHECK (reserved_seats BETWEEN 0 AND 1000);
ALTER TABLE world_addresses ADD COLUMN claimed_seats integer NOT NULL DEFAULT 0 CHECK (claimed_seats BETWEEN 0 AND reserved_seats);
