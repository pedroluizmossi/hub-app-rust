//! Provedores de dados compartilhados entre os mini-apps.
pub mod agents;
pub mod ai_usage;
pub mod docker;
pub mod gpu;
pub mod media;
pub mod ports;
pub mod sys;

use std::time::Duration;

use crate::core::bus::{Bus, Event, Topic};

/// Registra todos os provedores no barramento. Cada um só coleta enquanto algum
/// mini-app ligado o assina.
pub fn spawn_all(bus: &mut Bus) {
    let mut sys = sys::SysProvider::default();
    bus.spawn(Topic::Sys, Duration::from_secs(2), move || Event::Sys(sys.collect()));
    // NVIDIA: um `nvidia-smi` de vida longa entrega as leituras; aqui só pega a mais nova
    let mut gpu = gpu::GpuProvider::detect();
    bus.spawn(Topic::Gpu, Duration::from_secs(2), move || Event::Gpu(gpu.collect()));
    // `docker events` traz as mudanças; ps é rede de segurança e stats roda à parte
    let mut docker = docker::DockerProvider::default();
    bus.spawn_polled(Topic::Docker, Duration::from_secs(1), move |poker, poked| {
        docker.collect(poker, poked).map(Event::Docker)
    });
    // MPRIS: lê o player ativo a cada segundo e só publica o que mudou
    let mut media = media::MediaProvider::default();
    bus.spawn_polled(Topic::Media, Duration::from_secs(1), move |_, poked| media.collect(poked).map(Event::Media));
    let mut ai_usage = ai_usage::UsageProvider::default();
    bus.spawn_polled(Topic::AiUsage, Duration::from_secs(60), move |_, poked| {
        Some(Event::AiUsage(ai_usage.collect(poked)))
    });
    // `ss` é barato: a cada 3 s com a janela aberta, a cada 15 s escondida (só a bandeja vê)
    let mut last_ports: Option<std::time::Instant> = None;
    bus.spawn_polled(Topic::Ports, Duration::from_secs(3), move |poker, poked| {
        let due = last_ports.is_none_or(|t| t.elapsed() >= Duration::from_secs(15));
        if !(poked || poker.visible() || due) {
            return None;
        }
        last_ports = Some(std::time::Instant::now());
        Some(Event::Ports(ports::collect()))
    });
    // arquivos pequenos e duas consultas: a cada 2 s com a janela aberta, 10 s escondida
    let mut agents = agents::AgentsProvider::default();
    let mut last_agents: Option<std::time::Instant> = None;
    bus.spawn_polled(Topic::Agents, Duration::from_secs(2), move |poker, poked| {
        let due = last_agents.is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
        if !(poked || poker.visible() || due) {
            return None;
        }
        last_agents = Some(std::time::Instant::now());
        Some(Event::Agents(agents.collect()))
    });
}
