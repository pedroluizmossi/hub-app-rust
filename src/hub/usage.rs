//! Settings › System: what the hub itself uses now (CPU and memory), and each external app
//! running in a process of its own. Read every `EVERY` while the Settings tab is open (the
//! animation frame calls it); with the tab or the window closed, nothing is read.
//!
//! Plugins with a script and the built-in mini-apps run inside the hub: their use is part
//! of the hub's line.

use std::rc::Rc;
use std::time::{Duration, Instant};

use slint::{ModelRc, VecModel};

use super::Hub;
use crate::core::appdata::human;
use crate::core::i18n::{self, Language};
use crate::providers::procs::Meter;
use crate::tr;
use crate::ui::ProcUsage;

/// How often the numbers are read again.
const EVERY: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct UsageState {
    meter: Meter,
    last: Option<Instant>,
}

impl Hub {
    /// Reads and shows the usage, if `EVERY` has passed (or `now` is asked: the tab opened).
    pub(super) fn refresh_usage(&self, now: bool) {
        let mut st = self.usage.borrow_mut();
        if !now && st.last.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        st.last = Some(Instant::now());
        let me = std::process::id();
        let mut procs: Vec<(String, u32)> = vec![(crate::core::config::display_name(), me)];
        procs.extend(self.registry.all().iter().filter_map(|a| {
            let a = a.borrow();
            a.process_id().map(|pid| (a.manifest().name.clone(), pid))
        }));
        let pids: Vec<u32> = procs.iter().map(|(_, p)| *p).collect();
        let read = st.meter.read(&pids);
        drop(st);

        let mut rows = Vec::new();
        let (mut cpu_sum, mut mem_sum) = (0.0, 0);
        for (name, pid) in &procs {
            let Some(&(cpu, mem)) = read.get(pid) else { continue };
            cpu_sum += cpu;
            mem_sum += mem;
            rows.push(row(name, cpu, mem, false));
        }
        if rows.len() > 1 {
            rows.push(row(tr!("Total"), cpu_sum, mem_sum, true));
        }
        self.ui.set_usage_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    }
}

fn row(name: &str, cpu: f32, mem: u64, total: bool) -> ProcUsage {
    let mem = human(mem);
    let (mem_value, mem_unit) = mem.rsplit_once(' ').unwrap_or((&mem, ""));
    ProcUsage {
        name: name.into(),
        cpu,
        cpu_text: decimal(cpu).into(),
        mem_text: mem_value.into(),
        mem_unit: mem_unit.into(),
        total,
    }
}

/// "0.4" ("0,4" in Portuguese).
fn decimal(v: f32) -> String {
    let text = format!("{v:.1}");
    if i18n::current() == Language::PtBr { text.replace('.', ",") } else { text }
}
