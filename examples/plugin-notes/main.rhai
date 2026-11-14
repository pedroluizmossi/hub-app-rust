// Logic of the "Notes" plugin. The data lives in the app's SQLite database (db_batch,
// db_exec, db_query), which the hub opens in the app's data folder: it survives turning the
// app off and updating it.
//
// Texts: the view (view.slint) writes them with @tr("…") and translates them from
// i18n/pt-BR.po, so this script sends numbers and lets the view phrase them. Texts the
// script builds itself (the summary) go through tr("…", [args]), from the same file.

// The data came from an older version of the app ([data] version went up): the hub calls
// this before start(). An error here puts the data back as it was and the app does not start.
fn migrate(from, to) {
    // version 2: the "done" column
    if from < 2 {
        db_batch("ALTER TABLE notas ADD COLUMN feita INTEGER NOT NULL DEFAULT 0");
    }
    // version 3: table and columns renamed to English
    if from < 3 {
        db_batch(`ALTER TABLE notas RENAME TO notes;
            ALTER TABLE notes RENAME COLUMN texto TO text;
            ALTER TABLE notes RENAME COLUMN criada TO created;
            ALTER TABLE notes RENAME COLUMN feita TO done;`);
    }
}

fn start() {
    // a new install starts in the current format (no migration)
    // multi-line text in Rhai: between backticks
    db_batch(`CREATE TABLE IF NOT EXISTS notes (
        id INTEGER PRIMARY KEY,
        text TEXT NOT NULL,
        created INTEGER NOT NULL,
        done INTEGER NOT NULL DEFAULT 0
    )`);
    this.show();
}

fn action(name, arg) {
    if name == "add" {
        let text = arg;
        text.trim();   // trim() changes the text itself
        if text != "" {
            db_exec("INSERT INTO notes (text, created) VALUES (?, ?)", [text, now()]);
        }
    } else if name == "toggle" {
        db_exec("UPDATE notes SET done = 1 - done WHERE id = ?", [parse_int(arg)]);
    } else if name == "del" {
        db_exec("DELETE FROM notes WHERE id = ?", [parse_int(arg)]);
    } else if name == "clean" {
        db_exec("DELETE FROM notes WHERE done = 1");
    }
    this.show();
}

fn show() {
    let rows = [];
    for n in db_query("SELECT id, text, created, done FROM notes ORDER BY done, created DESC LIMIT 50") {
        // age in seconds: the view shows it as "now", "5 min ago"…
        rows.push(#{ id: `${n.id}`, text: n.text, done: n.done == 1, age: now() - n.created });
    }
    ui("rows", rows);
    let c = db_query("SELECT count(*) AS total, coalesce(sum(done), 0) AS done FROM notes")[0];
    let todo = c.total - c.done;
    ui("total", c.total);
    ui("todo", todo);
    ui("done", c.done);
    // the card's summary line (not translated: scripts have no catalog)
    summary(tr("{} to do", [todo]));
    mood(if c.total == 0 { "sleep" } else if todo == 0 { "happy" } else { "idle" });
}
