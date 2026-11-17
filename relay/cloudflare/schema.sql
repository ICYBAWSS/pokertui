CREATE TABLE IF NOT EXISTS players (
  pubkey     TEXT PRIMARY KEY,
  username   TEXT UNIQUE COLLATE NOCASE,
  state      TEXT,
  show_state INTEGER NOT NULL DEFAULT 0,
  listed     INTEGER NOT NULL DEFAULT 0,
  hands      INTEGER NOT NULL DEFAULT 0,
  won        INTEGER NOT NULL DEFAULT 0,
  net_bb     REAL    NOT NULL DEFAULT 0,
  rating     REAL    NOT NULL DEFAULT 1000,
  updated_at INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS hands (
  hand_hash    TEXT PRIMARY KEY,
  submitted_at INTEGER NOT NULL
);
-- Who has played whom (both directions), and how much rating `winner` has earned from `loser`.
CREATE TABLE IF NOT EXISTS pairs (
  winner TEXT NOT NULL,
  loser  TEXT NOT NULL,
  gained REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (winner, loser)
);
CREATE INDEX IF NOT EXISTS players_rating ON players (listed, rating DESC);
