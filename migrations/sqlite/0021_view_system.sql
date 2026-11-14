-- VIEW_SYSTEM (bit 22, 4194304) lets a user read the logs, jobs, scheduled tasks and backup list
-- without the admin right (RFX-84). The Manager role now includes it; users who already hold all
-- four manage rights (library, requests, users and issues, bits 1 to 4) get it too, so existing
-- manager accounts and their API keys can read them at once. Admins have every right already.
UPDATE users
SET permissions = permissions | 4194304
WHERE (permissions & 30) = 30
  AND (permissions & 1) = 0;
