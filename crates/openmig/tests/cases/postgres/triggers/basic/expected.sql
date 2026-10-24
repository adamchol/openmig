-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."posts"
CREATE TABLE "app"."posts" ("id" integer, "title" text, "updated_at" timestamp with time zone);
-- Create function "app"."touch"()
CREATE FUNCTION "app"."touch"() RETURNS trigger LANGUAGE plpgsql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$BEGIN NEW.updated_at := now(); RETURN NEW; END;$$;
-- Create trigger "posts_touch" of "app"."posts"
CREATE TRIGGER "posts_touch" BEFORE UPDATE OF "title" ON "app"."posts" FOR EACH ROW WHEN (old.title IS DISTINCT FROM new.title) EXECUTE FUNCTION "app"."touch"();
