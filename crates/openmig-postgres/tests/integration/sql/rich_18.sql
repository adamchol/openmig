CREATE TABLE app.shapes (w integer NOT NULL, h integer, area integer GENERATED ALWAYS AS (w * h) VIRTUAL);
ALTER TABLE app.shapes ADD CONSTRAINT shapes_w_check CHECK (w > 0) NOT ENFORCED;
