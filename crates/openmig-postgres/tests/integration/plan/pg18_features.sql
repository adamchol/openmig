-- Virtual generated columns and NOT ENFORCED constraints (PostgreSQL 18+).
-- min_major: 18
-- @both
CREATE SCHEMA app;
CREATE TABLE app.p (id integer PRIMARY KEY);
-- @a
CREATE TABLE app.t (w integer, h integer, area integer GENERATED ALWAYS AS (w * h) VIRTUAL, p_id integer);
ALTER TABLE app.t ADD CONSTRAINT t_w_check CHECK (w > 0);
-- @b
CREATE TABLE app.t (w integer, h integer, area integer GENERATED ALWAYS AS (w * h * 2) VIRTUAL, perimeter integer GENERATED ALWAYS AS (2 * (w + h)) VIRTUAL, p_id integer);
ALTER TABLE app.t ADD CONSTRAINT t_w_check CHECK (w > 0) NOT ENFORCED;
ALTER TABLE app.t ADD CONSTRAINT t_p_fk FOREIGN KEY (p_id) REFERENCES app.p (id) NOT ENFORCED;
