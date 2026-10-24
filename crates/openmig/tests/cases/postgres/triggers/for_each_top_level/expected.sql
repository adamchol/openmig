-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."a"
CREATE TABLE "app"."a" ("id" integer);
-- Create table "app"."b"
CREATE TABLE "app"."b" ("id" integer);
-- Create function "app"."audit"()
CREATE FUNCTION "app"."audit"() RETURNS trigger LANGUAGE plpgsql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$BEGIN RETURN NULL; END;$$;
-- Create trigger "audit" of "app"."a"
CREATE TRIGGER "audit" AFTER INSERT OR UPDATE OR DELETE ON "app"."a" FOR EACH STATEMENT EXECUTE FUNCTION "app"."audit"();
-- Create trigger "audit" of "app"."b"
CREATE TRIGGER "audit" AFTER INSERT OR UPDATE OR DELETE ON "app"."b" FOR EACH STATEMENT EXECUTE FUNCTION "app"."audit"();
