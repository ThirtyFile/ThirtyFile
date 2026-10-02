-- The schedulers of backup and replica policies look every few seconds for a policy's jobs that aren't over
-- (backups/policy.rs, replicas/policy.rs): by the set or the replica target, and the state. These indexes find them
-- without reading every job the policy ever ran, and replace the indexes by set and by policy alone. Finished jobs
-- are kept for a while as the history, then deleted (backups/runner.rs, `trim_histories`).
DROP INDEX backup_jobs_set;
CREATE INDEX backup_jobs_set ON backup_jobs (set_id, state, created_at);
DROP INDEX replica_jobs_policy;
CREATE INDEX replica_jobs_target ON replica_jobs (policy_id, location_id, state, created_at);
