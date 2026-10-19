-- Administrator alerts (Control panel › Administrator alerts)

-- What is wrong now (or was lately), one row for each: a storage location offline, the data disk filling up, a burst of
-- failed sign-ins, a certificate about to end. An alert is sent once when it starts (`announced`) and once when it is
-- over; `last_sent_at` is when anything was last sent about it, for the cooldown
CREATE TABLE alert_conditions (
    key TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    params TEXT NOT NULL DEFAULT '{}',
    since INTEGER NOT NULL,
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    announced INTEGER NOT NULL DEFAULT 0 CHECK (announced IN (0, 1)),
    last_sent_at INTEGER
);

-- Messages waiting to be sent, kept until they are, so a restart or an email or webhook server that is down loses none
-- of them: tried again with a longer wait each time, then given up on (`failed`)
CREATE TABLE alert_deliveries (
    id INTEGER PRIMARY KEY,
    channel TEXT NOT NULL CHECK (channel IN ('email', 'webhook')),
    -- The administrator an email is for
    user_id INTEGER,
    state TEXT NOT NULL CHECK (state IN ('firing', 'resolved')),
    kind TEXT NOT NULL,
    key TEXT NOT NULL,
    params TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    next_attempt_at INTEGER NOT NULL,
    done_at INTEGER,
    failed INTEGER NOT NULL DEFAULT 0 CHECK (failed IN (0, 1)),
    error TEXT
);
CREATE INDEX alert_deliveries_due ON alert_deliveries (next_attempt_at) WHERE done_at IS NULL AND failed = 0;
CREATE INDEX alert_deliveries_created ON alert_deliveries (created_at);
