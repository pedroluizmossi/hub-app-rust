// Logic of the "Top processes" plugin. It runs sandboxed: it can only use the MiniUtils API.
// The processes come from the hub's "procs" provider (uses in the manifest), every 3 s and
// the same on Linux and Windows: the plugin runs no program.
//
// Texts: the view (view.slint) phrases them with @tr("…"), and this script with tr("…"),
// both translated from i18n/pt-BR.po.

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
        // % of the whole machine: 25% on an 8-core machine is 2 full cores
        rows.push(#{ name: p.name, cpu: pct(p.cpu), mem: pct(p.mem), hot: p.cpu > 25.0 });
        if rows.len() >= 6 { break; }
    }
    ui("rows", rows);
    ui("count", this.data.count);
    let top = if this.data.by_cpu.len() > 0 { this.data.by_cpu[0].cpu } else { 0.0 };
    mood(if top > 50.0 { "busy" } else if top > 5.0 { "happy" } else { "idle" });
    if rows.len() > 0 {
        // the card's summary line, translated from i18n/pt-BR.po like the view's texts
        let line = if this.sort == "mem" {
            tr("{} {} memory", [rows[0].name, rows[0].mem])
        } else {
            tr("{} {} CPU", [rows[0].name, rows[0].cpu])
        };
        summary(line);
    }
}

// 12.345 -> "12.3%"
fn pct(v) {
    `${(v * 10.0).round() / 10.0}%`
}
