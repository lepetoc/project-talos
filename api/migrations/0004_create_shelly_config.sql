CREATE TABLE shelly_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    gateway_addr TEXT
);

INSERT INTO shelly_config (id, gateway_addr) VALUES (1, NULL);
