-- Create schema "app"
CREATE SCHEMA "app";
-- Create table "app"."orders"
CREATE TABLE "app"."orders" ("id" bigint GENERATED ALWAYS AS IDENTITY (START WITH 1000 INCREMENT BY 10 MINVALUE 1 MAXVALUE 9223372036854775807 CACHE 1 NO CYCLE) NOT NULL, "net" numeric NOT NULL, "vat" numeric NOT NULL, "gross" numeric GENERATED ALWAYS AS (net + vat) STORED);
