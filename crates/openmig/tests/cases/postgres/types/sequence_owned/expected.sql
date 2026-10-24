-- Create schema "app"
CREATE SCHEMA "app";
-- Create sequence "app"."ticket_numbers"
CREATE SEQUENCE "app"."ticket_numbers" AS integer START WITH 100 INCREMENT BY 5 MINVALUE 1 MAXVALUE 2147483647 CACHE 1 NO CYCLE;
-- Create sequence "app"."tickets_id_seq"
CREATE SEQUENCE "app"."tickets_id_seq" AS bigint START WITH 1 INCREMENT BY 1 MINVALUE 1 MAXVALUE 9223372036854775807 CACHE 1 NO CYCLE;
-- Create table "app"."tickets"
CREATE TABLE "app"."tickets" ("id" bigint DEFAULT nextval('app.tickets_id_seq'::regclass) NOT NULL, "number" integer DEFAULT nextval('app.ticket_numbers'::regclass));
-- Set owner column of sequence "app"."ticket_numbers"
ALTER SEQUENCE "app"."ticket_numbers" OWNED BY "app"."tickets"."number";
-- Set owner column of sequence "app"."tickets_id_seq"
ALTER SEQUENCE "app"."tickets_id_seq" OWNED BY "app"."tickets"."id";
