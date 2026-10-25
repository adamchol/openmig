-- Replace view "app"."kinds"
CREATE OR REPLACE VIEW `app`.`kinds` SQL SECURITY INVOKER COMMENT 'Event kinds' AS
SELECT kind, count() AS n FROM app.events GROUP BY kind;
-- Create view "app"."recent"
CREATE OR REPLACE VIEW `app`.`recent` AS
SELECT id, kind FROM app.events WHERE id > 100;
