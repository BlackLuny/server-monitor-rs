-- M5 was designed with SSH recording on-by-default, but the initial seed
-- in 20260424000000_initial.sql shipped with `false`. That made the new
-- in-browser replay UI look broken on a fresh install: every session
-- created a row but never wrote a .cast.
--
-- Flip the default to true for installs that haven't customized it. The
-- value-equality guard means operators who deliberately disabled
-- recording before this migration ran are left alone — they can keep
-- their choice.

UPDATE settings
   SET value = 'true'::jsonb
 WHERE key = 'ssh_recording_enabled'
   AND value = 'false'::jsonb;
