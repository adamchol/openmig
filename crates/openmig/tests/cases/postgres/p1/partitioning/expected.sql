-- Create table "app"."events_2027"
CREATE TABLE "app"."events_2027" PARTITION OF "app"."events" FOR VALUES FROM ('2027-01-01') TO ('2028-01-01');
