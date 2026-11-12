// Lógica do plugin "Notas". Os dados ficam num banco SQLite do app (db_batch, db_exec,
// db_query), que o hub abre na pasta de dados dele: sobrevive a desligar e a atualizar.

// Os dados vieram de uma versão antiga do app ([data] version subiu): o hub chama isto
// antes de start(). Um erro aqui devolve os dados como estavam e o app não começa.
fn migrate(de, para) {
    if de < 2 {
        db_batch("ALTER TABLE notas ADD COLUMN feita INTEGER NOT NULL DEFAULT 0");
    }
}

fn start() {
    // instalação nova já nasce no formato atual (sem migração)
    // texto de várias linhas no Rhai: entre crases
    db_batch(`CREATE TABLE IF NOT EXISTS notas (
        id INTEGER PRIMARY KEY,
        texto TEXT NOT NULL,
        criada INTEGER NOT NULL,
        feita INTEGER NOT NULL DEFAULT 0
    )`);
    this.show();
}

fn action(name, arg) {
    if name == "add" {
        let texto = arg;
        texto.trim();   // trim() muda o próprio texto
        if texto != "" {
            db_exec("INSERT INTO notas (texto, criada) VALUES (?, ?)", [texto, now()]);
        }
    } else if name == "toggle" {
        db_exec("UPDATE notas SET feita = 1 - feita WHERE id = ?", [parse_int(arg)]);
    } else if name == "del" {
        db_exec("DELETE FROM notas WHERE id = ?", [parse_int(arg)]);
    } else if name == "clean" {
        db_exec("DELETE FROM notas WHERE feita = 1");
    }
    this.show();
}

fn show() {
    let rows = [];
    for n in db_query("SELECT id, texto, criada, feita FROM notas ORDER BY feita, criada DESC LIMIT 50") {
        rows.push(#{ id: `${n.id}`, texto: n.texto, feita: n.feita == 1, quando: ago(n.criada) });
    }
    ui("rows", rows);
    let c = db_query("SELECT count(*) AS total, coalesce(sum(feita), 0) AS feitas FROM notas")[0];
    let falta = c.total - c.feitas;
    ui("subtitle", if c.total == 0 { "nenhuma nota" } else { `${falta} para fazer · ${c.feitas} feita(s)` });
    summary(`${falta} nota(s) para fazer`);
    mood(if c.total == 0 { "sleep" } else if falta == 0 { "happy" } else { "idle" });
}

// "agora", "há 5 min", "há 3 h", "há 2 d"
fn ago(t) {
    let s = now() - t;
    if s < 60 { "agora" } else if s < 3600 { `há ${s / 60} min` } else if s < 86400 { `há ${s / 3600} h` } else { `há ${s / 86400} d` }
}
