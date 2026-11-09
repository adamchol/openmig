-- Create sequence "app"."recordings_id_seq"
CREATE SEQUENCE "app"."recordings_id_seq" AS integer START WITH 1 INCREMENT BY 1 MINVALUE 1 MAXVALUE 2147483647 CACHE 1 NO CYCLE;
-- Drop primary key "recordings_pkey" of "app"."recordings"
ALTER TABLE "app"."recordings" DROP CONSTRAINT "recordings_pkey";
-- Drop index "recordings_tags" of "app"."recordings"
DROP INDEX "app"."recordings_tags";
-- Drop column "id" of "app"."recordings"
ALTER TABLE "app"."recordings" DROP COLUMN "id";
-- Add column "id" to table "app"."recordings"
ALTER TABLE "app"."recordings" ADD COLUMN "id" integer DEFAULT nextval('app.recordings_id_seq'::regclass) NOT NULL;
-- Set owner column of sequence "app"."recordings_id_seq"
ALTER SEQUENCE "app"."recordings_id_seq" OWNED BY "app"."recordings"."id";
-- Add primary key "recordings_pkey" of "app"."recordings"
ALTER TABLE "app"."recordings" ADD CONSTRAINT "recordings_pkey" PRIMARY KEY ("id");
-- Drop column "tags" of "app"."recordings"
ALTER TABLE "app"."recordings" DROP COLUMN "tags";
-- Add column "tags" to table "app"."recordings"
ALTER TABLE "app"."recordings" ADD COLUMN "tags" integer[];
-- Create index "recordings_tags" on "app"."recordings"
CREATE INDEX "recordings_tags" ON "app"."recordings" USING btree ("tags");
