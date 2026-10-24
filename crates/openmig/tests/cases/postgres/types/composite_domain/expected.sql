-- Create schema "app"
CREATE SCHEMA "app";
-- Create composite type "app"."money"
CREATE TYPE "app"."money" AS ("amount" numeric(12,2), "currency" character(3));
-- Create domain "app"."email"
CREATE DOMAIN "app"."email" AS text NOT NULL CONSTRAINT "has_at" CHECK (VALUE ~ '@'::text);
-- Create domain "app"."positive_money"
CREATE DOMAIN "app"."positive_money" AS app.money CONSTRAINT "positive" CHECK ((VALUE).amount > 0::numeric);
-- Create table "app"."payments"
CREATE TABLE "app"."payments" ("value" app.positive_money, "payer" app.email);
