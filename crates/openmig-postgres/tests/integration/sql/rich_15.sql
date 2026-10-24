CREATE TABLE app.devices (id integer PRIMARY KEY, serial text, CONSTRAINT devices_serial_key UNIQUE NULLS NOT DISTINCT (serial));
CREATE UNIQUE INDEX devices_lower_serial_idx ON app.devices (lower(serial)) NULLS NOT DISTINCT;
CREATE VIEW app.my_devices WITH (security_invoker = true) AS SELECT id FROM app.devices;
