CREATE TABLE sia_config (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    account TEXT,
    prefix TEXT,
    receiver_addr TEXT
);

INSERT INTO sia_config (id, account, prefix, receiver_addr) VALUES (1, NULL, NULL, NULL);
