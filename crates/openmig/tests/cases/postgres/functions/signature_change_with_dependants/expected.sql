-- Let SQL function bodies reference objects created later
SET check_function_bodies = off;
-- Drop the default of column "score" of "app"."players"
ALTER TABLE "app"."players" ALTER COLUMN "score" DROP DEFAULT;
-- Create function "app"."default_score"(bigint)
CREATE FUNCTION "app"."default_score"("base" bigint) RETURNS bigint LANGUAGE sql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$SELECT base * 2$$;
-- Alter column "score" of "app"."players"
ALTER TABLE "app"."players" ALTER COLUMN "score" TYPE bigint USING "score"::bigint;
-- Set the default of column "score" of "app"."players"
ALTER TABLE "app"."players" ALTER COLUMN "score" SET DEFAULT app.default_score(10::bigint);
-- Drop function "app"."default_score"(integer)
DROP FUNCTION "app"."default_score"(integer);
