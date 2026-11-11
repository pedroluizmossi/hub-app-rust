//! Processos que mais usam CPU e memória (para plugins: `uses = ["procs"]`).
//!
//! Existe para os plugins não dependerem do `ps`, que não há no Windows. O que eles
//! recebem (`ProcsSnapshot`) é igual nos dois sistemas; a coleta muda: `/proc` no Linux
//! (`linux.rs`), o crate `sysinfo` no Windows (`windows.rs`).
//!
//! CPU e memória são em % da máquina toda (0 a 100), como o `cpu` do provedor `sys`: um
//! processo que ocupa um núcleo inteiro numa máquina de 8 núcleos aparece com 12,5%.

#[cfg(target_os = "linux")]
#[path = "procs/linux.rs"]
mod imp;
#[cfg(windows)]
#[path = "procs/windows.rs"]
mod imp;

pub use imp::ProcsProvider;

/// Quantos processos vão em cada lista.
pub const TOP: usize = 10;

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ProcsSnapshot {
    /// Os que mais usam CPU, do maior para o menor (até `TOP`). Na primeira leitura todos
    /// estão em 0: o uso é medido entre duas leituras.
    pub by_cpu: Vec<Proc>,
    /// Os que mais usam memória, do maior para o menor (até `TOP`).
    pub by_mem: Vec<Proc>,
    /// Quantos processos havia (com memória própria; sem as threads do kernel).
    pub count: usize,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct Proc {
    pub pid: u32,
    /// Nome curto do programa ("firefox", sem caminho nem ".exe").
    pub name: String,
    /// % da CPU da máquina toda desde a leitura anterior.
    pub cpu: f32,
    /// % da memória da máquina.
    pub mem: f32,
    /// Memória residente em bytes.
    pub mem_bytes: u64,
}

/// Monta o snapshot a partir de todos os processos lidos: as duas listas, ordenadas e
/// cortadas em `TOP` (empate: o de menor pid primeiro, para a lista não pular à toa).
pub fn snapshot(all: Vec<Proc>) -> ProcsSnapshot {
    let count = all.len();
    let top = |key: fn(&Proc) -> f32| {
        let mut v = all.clone();
        v.sort_by(|a, b| key(b).total_cmp(&key(a)).then(a.pid.cmp(&b.pid)));
        v.truncate(TOP);
        v
    };
    ProcsSnapshot { by_cpu: top(|p| p.cpu), by_mem: top(|p| p.mem), count }
}

/// Nome para mostrar no Windows: sem pasta e sem ".exe" (o nome vem como "chrome.exe").
/// O Linux não precisa: o `comm` já é curto (e pode ter "/", como em "kworker/0:1").
#[cfg(any(windows, test))]
pub fn short_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    match base.len().checked_sub(4) {
        Some(cut) if base.is_char_boundary(cut) && base[cut..].eq_ignore_ascii_case(".exe") => base[..cut].to_string(),
        _ => base.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, cpu: f32, mem: f32) -> Proc {
        Proc { pid, name: format!("p{pid}"), cpu, mem, mem_bytes: 0 }
    }

    #[test]
    fn ordena_e_corta_as_duas_listas() {
        let all: Vec<Proc> = (1..=15).map(|i| p(i, i as f32, (16 - i) as f32)).collect();
        let s = snapshot(all);
        assert_eq!(s.count, 15);
        assert_eq!(s.by_cpu.len(), TOP);
        assert_eq!(s.by_cpu[0].pid, 15);
        assert_eq!(s.by_mem[0].pid, 1);
        assert!(s.by_cpu.windows(2).all(|w| w[0].cpu >= w[1].cpu));
    }

    #[test]
    fn empate_fica_pelo_pid() {
        let s = snapshot(vec![p(9, 0.0, 0.0), p(3, 0.0, 0.0), p(5, 0.0, 0.0)]);
        let pids: Vec<u32> = s.by_cpu.iter().map(|p| p.pid).collect();
        assert_eq!(pids, [3, 5, 9]);
    }

    #[test]
    fn nome_curto() {
        assert_eq!(short_name("chrome.exe"), "chrome");
        assert_eq!(short_name("C:\\Windows\\explorer.EXE"), "explorer");
        assert_eq!(short_name("/usr/bin/firefox"), "firefox");
        assert_eq!(short_name("exe"), "exe");
        assert_eq!(short_name("ação.exe"), "ação");
    }
}
