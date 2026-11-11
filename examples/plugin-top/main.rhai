// Lógica do plugin "Top processos". Roda isolado: só pode usar a API do MiniUtils.
// Os processos chegam do provedor "procs" do hub (uses no manifesto), a cada 3 s e iguais
// no Linux e no Windows: o plugin não roda nenhum programa.

fn start() {
    this.sort = "cpu";
    this.data = ();
}

fn action(name, arg) {
    if name == "sort" {
        this.sort = arg;
        ui("sort", arg);
        this.show();
    }
}

fn on_event(topic, data) {
    if topic == "procs" {
        this.data = data;
        this.show();
    }
}

fn show() {
    if type_of(this.data) == "()" {
        return;
    }
    let list = if this.sort == "mem" { this.data.by_mem } else { this.data.by_cpu };
    let rows = [];
    for p in list {
        // % da máquina toda: 25% numa máquina de 8 núcleos são 2 núcleos inteiros
        rows.push(#{ name: p.name, cpu: pct(p.cpu), mem: pct(p.mem), hot: p.cpu > 25.0 });
        if rows.len() >= 6 { break; }
    }
    ui("rows", rows);
    let por = if this.sort == "mem" { "por memória" } else { "por CPU" };
    ui("subtitle", `${por} · ${this.data.count} processos`);
    let top = if this.data.by_cpu.len() > 0 { this.data.by_cpu[0].cpu } else { 0.0 };
    mood(if top > 50.0 { "busy" } else if top > 5.0 { "happy" } else { "idle" });
    if rows.len() > 0 {
        let de = if this.sort == "mem" { " de memória" } else { " de CPU" };
        let valor = if this.sort == "mem" { rows[0].mem } else { rows[0].cpu };
        summary(rows[0].name + " " + valor + de);
    }
}

// 12.345 -> "12.3%"
fn pct(v) {
    `${(v * 10.0).round() / 10.0}%`
}
