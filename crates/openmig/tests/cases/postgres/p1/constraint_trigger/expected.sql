-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."ledger"
CREATE TABLE "app"."ledger" ("amount" numeric);
-- Create function "app"."check_balance"()
CREATE FUNCTION "app"."check_balance"() RETURNS trigger LANGUAGE plpgsql VOLATILE CALLED ON NULL INPUT SECURITY INVOKER PARALLEL UNSAFE COST 100 AS $$BEGIN RETURN NULL; END;$$;
-- Create trigger "ledger_balance" of "app"."ledger"
CREATE CONSTRAINT TRIGGER "ledger_balance" AFTER INSERT OR UPDATE ON "app"."ledger" DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION "app"."check_balance"();
