CREATE TABLE snapshots (
        snapshot_id    TEXT PRIMARY KEY,
        content        TEXT NOT NULL,
        app            TEXT NOT NULL,
        window_title   TEXT NOT NULL,
        url            TEXT,
        domain         TEXT,
        captured_at    INTEGER NOT NULL,
        last_seen_at   INTEGER NOT NULL,
        duration_s     REAL    NOT NULL DEFAULT 0,
        sighting_count INTEGER NOT NULL DEFAULT 1,
        schema_v       INTEGER NOT NULL DEFAULT 1
    , focused_name TEXT, focused_role TEXT, focused_path TEXT);

CREATE TABLE activity_events (
        event_id        INTEGER PRIMARY KEY AUTOINCREMENT,
        snapshot_id     TEXT    NOT NULL REFERENCES snapshots(snapshot_id) ON DELETE CASCADE,
        app             TEXT    NOT NULL,
        window_title    TEXT    NOT NULL,
        url             TEXT,
        domain          TEXT,
        started_at      INTEGER NOT NULL,
        last_seen_at    INTEGER NOT NULL,
        duration_s      REAL    NOT NULL DEFAULT 0,
        content_excerpt TEXT    NOT NULL DEFAULT '',
        focused_name    TEXT,
        focused_role    TEXT,
        focused_path    TEXT
    );

CREATE TABLE working_memory (
        wm_id       INTEGER PRIMARY KEY AUTOINCREMENT,
        snapshot_id TEXT    NOT NULL REFERENCES snapshots(snapshot_id),
        added_at    INTEGER NOT NULL,
        relevance   REAL    NOT NULL DEFAULT 1.0
    );

CREATE TABLE "chronicle" (
        chronicle_id  TEXT PRIMARY KEY,
        level         TEXT    NOT NULL,
        period_key    TEXT    NOT NULL,
        summary_text  TEXT    NOT NULL,
        snapshot_ids  TEXT    NOT NULL DEFAULT '[]',
        child_ids     TEXT    NOT NULL DEFAULT '[]',
        token_count   INTEGER,
        generated_at  INTEGER NOT NULL,
        model_used    TEXT,
        is_dirty      INTEGER NOT NULL DEFAULT 0
    );

CREATE TABLE index_entries (
        id              INTEGER PRIMARY KEY AUTOINCREMENT,
        topic           TEXT    NOT NULL,
        entities        TEXT,
        domain          TEXT,
        snapshot_ids    TEXT    NOT NULL DEFAULT '[]',
        created_at      INTEGER NOT NULL,
        last_updated_at INTEGER NOT NULL DEFAULT 0
    );

CREATE TABLE kg_entities (
        entity_id   TEXT PRIMARY KEY,
        name        TEXT    NOT NULL,
        entity_type TEXT    NOT NULL,
        first_seen  INTEGER NOT NULL,
        last_seen   INTEGER NOT NULL
    );

CREATE TABLE kg_relations (
        relation_id        TEXT PRIMARY KEY,
        subject_id         TEXT NOT NULL REFERENCES kg_entities(entity_id),
        predicate          TEXT NOT NULL,
        object_id          TEXT NOT NULL REFERENCES kg_entities(entity_id),
        valid_from         INTEGER NOT NULL,
        valid_to           INTEGER,
        source_snapshot_id TEXT REFERENCES snapshots(snapshot_id)
    );

CREATE TABLE workflows (
        workflow_id        TEXT PRIMARY KEY,
        name               TEXT NOT NULL,
        excerpt            TEXT NOT NULL DEFAULT '',
        apps               TEXT NOT NULL DEFAULT '[]',
        frequency_label    TEXT NOT NULL DEFAULT '',
        body_json          TEXT NOT NULL DEFAULT '[]',
        status             TEXT NOT NULL DEFAULT 'proposed',
        source             TEXT NOT NULL DEFAULT 'detected',
        confidence         REAL NOT NULL DEFAULT 0.5,
        detected_at_level  TEXT NOT NULL DEFAULT 'day',
        first_detected_at  INTEGER NOT NULL,
        last_detected_at   INTEGER NOT NULL,
        generated_at       INTEGER NOT NULL,
        source_from        TEXT,
        source_to          TEXT
    );

CREATE TABLE nudges (
        nudge_id        TEXT PRIMARY KEY,
        kind            TEXT NOT NULL,        -- morning_brief | evening_wrap | contextual_nudge | scheduled_rule
        dedupe_key      TEXT,                 -- e.g. "morning_brief:2026-04-23" — one row per key
        scheduled_for   INTEGER NOT NULL,     -- unix ts when this should surface
        title           TEXT NOT NULL,
        body            TEXT NOT NULL,
        deep_link       TEXT,                 -- e.g. "woof://chat?prompt=..." or "woof://memory-hub/timeline"
        status          TEXT NOT NULL DEFAULT 'pending',  -- pending | ready | seen | dismissed | skipped
        created_at      INTEGER NOT NULL,
        sent_at         INTEGER,
        seen_at         INTEGER,
        dismissed_at    INTEGER,
        meta_json       TEXT NOT NULL DEFAULT '{}'
    );

CREATE TABLE proactive_rules (
        rule_id          TEXT PRIMARY KEY,
        label            TEXT NOT NULL,
        prompt           TEXT NOT NULL,
        schedule_kind    TEXT NOT NULL DEFAULT 'daily',
        days_of_week     TEXT NOT NULL DEFAULT '',
        hour             INTEGER NOT NULL DEFAULT 9,
        minute           INTEGER NOT NULL DEFAULT 0,
        interval_minutes INTEGER NOT NULL DEFAULT 0,
        timezone         TEXT NOT NULL DEFAULT 'local',
        enabled          INTEGER NOT NULL DEFAULT 1,
        created_at       INTEGER NOT NULL,
        updated_at       INTEGER NOT NULL,
        last_fired_at    INTEGER,
        fire_at          INTEGER
    );

CREATE TABLE recording_events (
        event_id            INTEGER PRIMARY KEY AUTOINCREMENT,
        session_started_at  INTEGER NOT NULL,
        ts_ms               INTEGER NOT NULL,
        kind                TEXT    NOT NULL,
        button              TEXT,
        element_name        TEXT,
        element_role        TEXT,
        element_path        TEXT,
        app                 TEXT,
        window_title        TEXT
    );

CREATE TABLE inline_rewrite_uses (
        app            TEXT NOT NULL,
        domain         TEXT NOT NULL DEFAULT '',
        first_used_at  INTEGER NOT NULL,
        last_used_at   INTEGER NOT NULL,
        use_count      INTEGER NOT NULL DEFAULT 1,
        PRIMARY KEY (app, domain)
    );

CREATE TABLE wiki_pages (
        slug          TEXT PRIMARY KEY,
        page_type     TEXT    NOT NULL,
        title         TEXT    NOT NULL,
        aliases       TEXT    NOT NULL DEFAULT '',
        summary       TEXT    NOT NULL DEFAULT '',
        body          TEXT    NOT NULL DEFAULT '',
        links         TEXT    NOT NULL DEFAULT '[]',
        snapshot_ids  TEXT    NOT NULL DEFAULT '[]',
        mention_count INTEGER NOT NULL DEFAULT 0,
        first_seen    INTEGER NOT NULL,
        last_seen     INTEGER NOT NULL,
        is_dirty      INTEGER NOT NULL DEFAULT 0,
        updated_at    INTEGER NOT NULL,
        model_used    TEXT
    );

CREATE TABLE salient_flags (
        flag_id      INTEGER PRIMARY KEY AUTOINCREMENT,
        kind         TEXT    NOT NULL,
        text         TEXT    NOT NULL,
        snapshot_id  TEXT,
        period_key   TEXT    NOT NULL,
        status       TEXT    NOT NULL DEFAULT 'open',
        created_at   INTEGER NOT NULL
    );

CREATE TABLE chat_turns (
        turn_id    INTEGER PRIMARY KEY AUTOINCREMENT,
        thread_id  TEXT,
        role       TEXT    NOT NULL,
        content    TEXT    NOT NULL,
        created_at INTEGER NOT NULL,
        consumed   INTEGER NOT NULL DEFAULT 0
    );

CREATE TABLE inline_outputs (
        output_id   INTEGER PRIMARY KEY AUTOINCREMENT,
        app         TEXT    NOT NULL DEFAULT '',
        domain      TEXT    NOT NULL DEFAULT '',
        instruction TEXT    NOT NULL DEFAULT '',
        output      TEXT    NOT NULL,
        created_at  INTEGER NOT NULL
    );

CREATE TABLE time_ledger (
        hour_ts INTEGER NOT NULL,
        app     TEXT    NOT NULL,
        domain  TEXT    NOT NULL DEFAULT '',
        title   TEXT    NOT NULL DEFAULT '',
        seconds REAL    NOT NULL DEFAULT 0,
        PRIMARY KEY (hour_ts, app, domain, title)
    );

CREATE TABLE time_rules (
        rule_id        INTEGER PRIMARY KEY AUTOINCREMENT,
        project        TEXT    NOT NULL,
        app            TEXT,
        domain         TEXT,
        title_contains TEXT,
        source         TEXT    NOT NULL DEFAULT 'user',
        created_at     INTEGER NOT NULL
    );

CREATE TABLE style_notes (
        note_id    INTEGER PRIMARY KEY AUTOINCREMENT,
        source     TEXT    NOT NULL,
        surface    TEXT    NOT NULL,
        bullet     TEXT    NOT NULL,
        updated_at INTEGER NOT NULL
    );

CREATE VIRTUAL TABLE snapshots_fts USING fts5(
        content, app, window_title, domain,
        content='snapshots', content_rowid='rowid'
    );

CREATE VIRTUAL TABLE index_fts USING fts5(
        topic, entities, domain,
        content='index_entries', content_rowid='id'
    );

CREATE VIRTUAL TABLE wiki_fts USING fts5(
        title, aliases, summary, body,
        content='wiki_pages', content_rowid='rowid'
    );

CREATE TRIGGER snapshots_ai AFTER INSERT ON snapshots BEGIN
        INSERT INTO snapshots_fts(rowid, content, app, window_title, domain)
        VALUES (new.rowid, new.content, new.app, new.window_title, new.domain);
    END;

CREATE TRIGGER snapshots_au AFTER UPDATE ON snapshots BEGIN
        INSERT INTO snapshots_fts(snapshots_fts, rowid, content, app, window_title, domain)
        VALUES ('delete', old.rowid, old.content, old.app, old.window_title, old.domain);
        INSERT INTO snapshots_fts(rowid, content, app, window_title, domain)
        VALUES (new.rowid, new.content, new.app, new.window_title, new.domain);
    END;

CREATE TRIGGER index_entries_ai AFTER INSERT ON index_entries BEGIN
        INSERT INTO index_fts(rowid, topic, entities, domain)
        VALUES (new.id, new.topic, new.entities, new.domain);
    END;

CREATE TRIGGER index_entries_ad AFTER DELETE ON index_entries BEGIN
        INSERT INTO index_fts(index_fts, rowid, topic, entities, domain)
        VALUES ('delete', old.id, old.topic, old.entities, old.domain);
    END;

CREATE TRIGGER index_entries_au AFTER UPDATE ON index_entries BEGIN
        INSERT INTO index_fts(index_fts, rowid, topic, entities, domain)
        VALUES ('delete', old.id, old.topic, old.entities, old.domain);
        INSERT INTO index_fts(rowid, topic, entities, domain)
        VALUES (new.id, new.topic, new.entities, new.domain);
    END;

CREATE TRIGGER wiki_pages_ai AFTER INSERT ON wiki_pages BEGIN
        INSERT INTO wiki_fts(rowid, title, aliases, summary, body)
        VALUES (new.rowid, new.title, new.aliases, new.summary, new.body);
    END;

CREATE TRIGGER wiki_pages_ad AFTER DELETE ON wiki_pages BEGIN
        INSERT INTO wiki_fts(wiki_fts, rowid, title, aliases, summary, body)
        VALUES ('delete', old.rowid, old.title, old.aliases, old.summary, old.body);
    END;

CREATE TRIGGER wiki_pages_au AFTER UPDATE ON wiki_pages BEGIN
        INSERT INTO wiki_fts(wiki_fts, rowid, title, aliases, summary, body)
        VALUES ('delete', old.rowid, old.title, old.aliases, old.summary, old.body);
        INSERT INTO wiki_fts(rowid, title, aliases, summary, body)
        VALUES (new.rowid, new.title, new.aliases, new.summary, new.body);
    END;

CREATE INDEX idx_snap_captured ON snapshots(captured_at);
CREATE INDEX idx_snap_app      ON snapshots(app);
CREATE INDEX idx_snap_domain   ON snapshots(domain);
CREATE INDEX idx_activity_snapshot
        ON activity_events(snapshot_id, event_id DESC);
CREATE INDEX idx_activity_last_seen
        ON activity_events(last_seen_at DESC);
CREATE INDEX idx_wm_added_at  ON working_memory(added_at);
CREATE INDEX idx_wm_relevance ON working_memory(relevance DESC);
CREATE INDEX idx_chron_level ON chronicle(level);
CREATE UNIQUE INDEX idx_chron_level_period ON chronicle(level, period_key);
CREATE INDEX idx_idx_entries_domain ON index_entries(domain);
CREATE UNIQUE INDEX idx_idx_entries_topic_domain
        ON index_entries(topic, COALESCE(domain, ''));
CREATE UNIQUE INDEX idx_kg_name_type ON kg_entities(name, entity_type);
CREATE INDEX idx_kg_subj ON kg_relations(subject_id);
CREATE INDEX idx_kg_pred ON kg_relations(predicate);
CREATE INDEX idx_wf_status     ON workflows(status);
CREATE INDEX idx_wf_last_seen  ON workflows(last_detected_at DESC);
CREATE UNIQUE INDEX idx_wf_name_lower ON workflows(lower(name));
CREATE INDEX idx_nudges_status     ON nudges(status);
CREATE INDEX idx_nudges_scheduled  ON nudges(scheduled_for);
CREATE UNIQUE INDEX idx_nudges_dedupe ON nudges(dedupe_key) WHERE dedupe_key IS NOT NULL;
CREATE INDEX idx_prules_enabled ON proactive_rules(enabled);
CREATE INDEX idx_recev_session ON recording_events(session_started_at, ts_ms);
CREATE INDEX idx_wiki_type      ON wiki_pages(page_type);
CREATE INDEX idx_wiki_dirty     ON wiki_pages(is_dirty);
CREATE INDEX idx_wiki_last_seen ON wiki_pages(last_seen DESC);
CREATE INDEX idx_flags_kind_status ON salient_flags(kind, status);
CREATE INDEX idx_flags_created     ON salient_flags(created_at DESC);
CREATE UNIQUE INDEX idx_flags_dedupe
        ON salient_flags(period_key, kind, text);
CREATE INDEX idx_chat_turns_unconsumed
        ON chat_turns(consumed, created_at);
CREATE INDEX idx_chat_turns_created ON chat_turns(created_at DESC);
CREATE INDEX idx_inline_outputs_created
        ON inline_outputs(created_at DESC);
CREATE INDEX idx_time_ledger_hour ON time_ledger(hour_ts);
CREATE INDEX idx_style_notes_source_surface
        ON style_notes(source, surface);

PRAGMA user_version = 18;
PRAGMA schema_version = 92;
