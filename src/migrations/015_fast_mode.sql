-- NULL inherits the operator's global Fast policy. Existing tokens inherit.
ALTER TABLE access_tokens ADD COLUMN fast_mode INTEGER CHECK(fast_mode IN (0,1));
-- Reversible pause; distinct from irrevocable revoked_at.
ALTER TABLE access_tokens ADD COLUMN disabled INTEGER NOT NULL DEFAULT 0 CHECK(disabled IN (0,1));
