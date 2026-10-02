ALTER TABLE oauth_models ADD COLUMN metadata TEXT;
-- Old ID-only rows cannot supply an authoritative client catalog.
UPDATE oauth_accounts SET catalog_updated_at=NULL;
