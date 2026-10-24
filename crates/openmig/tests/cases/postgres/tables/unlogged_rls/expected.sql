-- Set replica identity of table "app"."events"
ALTER TABLE "app"."events" REPLICA IDENTITY FULL;
-- Enable row level security on table "app"."events"
ALTER TABLE "app"."events" ENABLE ROW LEVEL SECURITY;
-- Force row level security on table "app"."events"
ALTER TABLE "app"."events" FORCE ROW LEVEL SECURITY;
