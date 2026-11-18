//! Data providers shared by the mini-apps.
pub mod agents;
pub mod ai_usage;
pub mod docker;
pub mod gpu;
pub mod media;
pub mod ports;
pub mod procs;
pub mod sys;

use std::time::Duration;

use crate::core::bus::{Bus, Event, Topic};

/// Registers every provider on the bus. Each one only collects while some enabled mini-app
/// subscribes to it.
pub fn spawn_all(bus: &mut Bus) {
    let mut sys = sys::SysProvider::default();
    bus.spawn(Topic::Sys, Duration::from_secs(2), move || Event::Sys(sys.collect()));
    // NVIDIA: a long-lived `nvidia-smi` delivers the readings; here we just take the newest
    let mut gpu = gpu::GpuProvider::detect();
    bus.spawn(Topic::Gpu, Duration::from_secs(2), move || Event::Gpu(gpu.collect()));
    // `docker events` brings the changes; ps is a safety net and stats runs separately
    let mut docker = docker::DockerProvider::default();
    bus.spawn_polled(Topic::Docker, Duration::from_secs(1), move |poker, poked| {
        docker.collect(poker, poked).map(Event::Docker)
    });
    // MPRIS: reads the active player every second and only publishes what changed
    let mut media = media::MediaProvider::default();
    bus.spawn_polled(Topic::Media, Duration::from_secs(1), move |_, poked| media.collect(poked).map(Event::Media));
    let mut ai_usage = ai_usage::UsageProvider::default();
    // only publishes when some source was read (or a reading was requested)
    bus.spawn_polled(Topic::AiUsage, Duration::from_secs(60), move |_, poked| ai_usage.collect(poked).map(Event::AiUsage));
    // `ss` is cheap: every 3 s with the window open, every 15 s hidden (only the tray sees it)
    let mut last_ports: Option<std::time::Instant> = None;
    bus.spawn_polled(Topic::Ports, Duration::from_secs(3), move |poker, poked| {
        let due = last_ports.is_none_or(|t| t.elapsed() >= Duration::from_secs(15));
        if !(poked || poker.visible() || due) {
            return None;
        }
        last_ports = Some(std::time::Instant::now());
        Some(Event::Ports(ports::collect()))
    });
    // small files and two queries: every 2 s with the window (or a session's detail window)
    // open, 10 s hidden
    let mut agents = agents::AgentsProvider::default();
    let mut last_agents: Option<std::time::Instant> = None;
    bus.spawn_polled(Topic::Agents, Duration::from_secs(2), move |poker, poked| {
        let due = last_agents.is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
        if !(poked || poker.visible() || agents::watching() || due) {
            return None;
        }
        last_agents = Some(std::time::Instant::now());
        Some(Event::Agents(agents.collect()))
    });
    // reading every process costs more than the rest: every 3 s, and only with a plugin
    // subscribed
    let mut procs = procs::ProcsProvider::default();
    bus.spawn(Topic::Procs, Duration::from_secs(3), move || Event::Procs(procs.collect()));
}
