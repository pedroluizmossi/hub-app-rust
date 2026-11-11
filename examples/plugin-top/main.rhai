// Lógica do plugin "Top processos". Roda isolado: só pode usar a API do MiniUtils
// (ui, mood, summary, run com os programas do manifesto, notify, now).

fn start() {
    this.sort = "cpu";
    this.refresh();
}

fn tick() {
    this.refresh();
}

fn action(name, arg) {
    if name == "sort" {
        this.sort = arg;
        ui("sort", arg);
    }
    this.refresh();
}

fn refresh() {
    let key = if this.sort == "mem" { "-pmem" } else { "-pcpu" };
    let r = run(["ps", "-eo", "comm,pcpu,pmem", "--sort=" + key, "--no-headers"]);
    if !r.ok {
        mood("alarm");
        ui("subtitle", "ps falhou: " + r.err);
        return;
    }
    let rows = [];
    let top = 0.0;
    for line in r.out.split("\n") {
        let parts = line.split();
        if parts.len() < 3 { continue; }
        let n = parts.len();
        let cpu = parse_float(parts[n - 2]);
        let mem = parse_float(parts[n - 1]);
        if rows.len() == 0 { top = cpu; }
        rows.push(#{ name: parts[0], cpu: parts[n - 2] + "%", mem: parts[n - 1] + "%", hot: cpu > 50.0 });
        if rows.len() >= 6 { break; }
    }
    ui("rows", rows);
    let por = if this.sort == "mem" { "por memória" } else { "por CPU" };
    ui("subtitle", por + " · a cada 5 s");
    mood(if top > 80.0 { "busy" } else if top > 15.0 { "happy" } else { "idle" });
    if rows.len() > 0 {
        summary(rows[0].name + " " + rows[0].cpu + " de CPU");
    }
}
