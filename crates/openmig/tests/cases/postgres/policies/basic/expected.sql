-- Alter policy "notes_select" of "app"."notes"
ALTER POLICY "notes_select" ON "app"."notes" TO "app_reader" USING (owner = CURRENT_USER);
-- Create policy "notes_write" of "app"."notes"
CREATE POLICY "notes_write" ON "app"."notes" AS PERMISSIVE FOR INSERT TO "app_writer" WITH CHECK (owner = CURRENT_USER);
