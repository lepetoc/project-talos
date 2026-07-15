CREATE TABLE sensor_mappings (
    sensor_id TEXT PRIMARY KEY,
    zone_id INTEGER NOT NULL
);

-- Placeholder row: replace 'placeholder-sensor-id' with the real Shelly
-- sensor identifier once it is known, then remove this comment.
INSERT INTO sensor_mappings (sensor_id, zone_id) VALUES ('placeholder-sensor-id', 1);
