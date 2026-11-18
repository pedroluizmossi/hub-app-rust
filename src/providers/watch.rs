//! Following a command that prints a line for each change (`docker events`,
//! `kubectl get --watch-only`): each relevant line asks its provider for a reading.

use std::io::{BufRead, BufReader};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use crate::core::bus::Poker;

/// Wait before starting the command again: grows from 2 s up to 30 s.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MAX: Duration = Duration::from_secs(30);
/// Bursts of lines (e.g. `compose up`) become a single reading.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// How often the idle thread checks whether it still has subscribers (the delay before it
/// stops once the app is turned off).
const IDLE_CHECK: Duration = Duration::from_secs(1);

/// Runs `program args` while the provider has subscribers and pokes it (once per burst) for
/// each output line `relevant` accepts; if the command ends, starts it again after a wait
/// that grows from 2 s to 30 s. Clears `watching` when it stops (no subscribers, or the
/// program vanished), so the provider knows to start it again.
pub fn follow(poker: Poker, watching: Arc<AtomicBool>, program: &str, args: &[String], relevant: fn(&str) -> bool) {
    let mut retry = RETRY_FIRST;
    let mut first = true;
    while poker.subscribed() {
        let started = Instant::now();
        let spawned = crate::core::exec::command(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            // the command vanished: `collect` decides to watch again, once ps answers
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => {
                if !sleep_subscribed(&poker, retry) {
                    break;
                }
                retry = (retry * 2).min(RETRY_MAX);
                continue;
            }
        };
        // reconnected: changes missed while it was down are picked up by a reading
        if !first {
            poker.poke();
        }
        first = false;

        if let Some(stdout) = child.stdout.take() {
            let (tx, rx) = mpsc::channel::<String>();
            std::thread::spawn(move || {
                for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                    if tx.send(line).is_err() {
                        break;
                    }
                }
            });
            let mut due: Option<Instant> = None;
            loop {
                if !poker.subscribed() {
                    break;
                }
                // waits for the pending debounce, or else up to IDLE_CHECK to see the subscription
                let wait = due.map_or(IDLE_CHECK, |t| t.saturating_duration_since(Instant::now()));
                match rx.recv_timeout(wait) {
                    Ok(line) => {
                        if relevant(&line) && due.is_none() {
                            due = Some(Instant::now() + DEBOUNCE);
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break, // the command ended
                }
                if due.is_some_and(|t| Instant::now() >= t) {
                    due = None;
                    poker.poke();
                }
            }
        }
        let _ = child.kill();
        let _ = child.wait();

        // it ran for a good while: the next drop starts waiting from 2 s again
        if started.elapsed() >= RETRY_MAX {
            retry = RETRY_FIRST;
        }
        if !sleep_subscribed(&poker, retry) {
            break;
        }
        retry = (retry * 2).min(RETRY_MAX);
    }
    watching.store(false, Ordering::Relaxed);
}

/// Sleeps `d`, unless the provider loses its subscribers. `false` if it lost them.
fn sleep_subscribed(poker: &Poker, d: Duration) -> bool {
    let end = Instant::now() + d;
    loop {
        if !poker.subscribed() {
            return false;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(IDLE_CHECK));
    }
}

