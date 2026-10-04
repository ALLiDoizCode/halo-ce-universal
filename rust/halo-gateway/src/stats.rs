//! What the gateway counts about itself, for the operator's log and for the
//! tests.

use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Mutex;

/// Keep at most this many timings (about 55 minutes of ticks); older ones are dropped.
const MAX_TIMINGS: usize = 100_000;

#[derive(Debug, Default)]
pub struct Stats {
    /// Ticks the gateway saw complete.
    pub ticks: AtomicU64,
    /// Ticks that never reached the gateway (a gap in the tick numbers).
    pub ticks_skipped: AtomicU64,
    /// Calls to the module's `submit_inputs`, and the inputs in them.
    pub batches_submitted: AtomicU64,
    pub inputs_submitted: AtomicU64,
    /// Input datagrams that parsed.
    pub inputs_received: AtomicU64,
    /// ... dropped because a newer one was already held.
    pub inputs_late: AtomicU64,
    /// ... dropped because they did not come from the address bound to their player.
    pub inputs_unbound: AtomicU64,
    pub hellos: AtomicU64,
    /// Challenges sent in answer to Hellos.
    pub challenges: AtomicU64,
    /// Auths accepted (a resend of one counts again), and refused.
    pub auths_accepted: AtomicU64,
    pub auths_refused: AtomicU64,
    /// Players unbound because they were silent for the idle timeout.
    pub sessions_expired: AtomicU64,
    /// Datagrams that did not parse.
    pub malformed: AtomicU64,
    pub datagrams_sent: AtomicU64,
    /// Bytes sent, counting IP and UDP headers.
    pub wire_bytes_sent: AtomicU64,
    pub states_sent: AtomicU64,
    pub send_errors: AtomicU64,
    /// Ticks that were still being sent when the next arrived.
    pub send_overruns: AtomicU64,
    /// Datagrams sent by each sending thread.
    pub per_thread_datagrams: Vec<AtomicU64>,
    timings: Mutex<Timings>,
}

#[derive(Debug, Default)]
struct Timings {
    /// Microseconds from a tick reaching the gateway to its last datagram being sent.
    send_us: Vec<u32>,
    /// The same since `take_send_window` was last called.
    send_window_us: Vec<u32>,
    /// Microseconds from the module stamping a tick to it reaching the gateway.
    arrival_us: Vec<i64>,
}

/// Percentiles of a list of timings.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Spread {
    pub p50: f64,
    pub p99: f64,
    pub max: f64,
}

impl Spread {
    pub fn of(mut values: Vec<f64>) -> Spread {
        values.sort_by(f64::total_cmp);
        let at = |p: f64| values.get(((values.len().max(1) - 1) as f64 * p).round() as usize).copied().unwrap_or(0.0);
        Spread { p50: at(0.5), p99: at(0.99), max: at(1.0) }
    }
}

/// The counters at one moment, plus the timings so far as milliseconds.
#[derive(Debug, Clone)]
pub struct StatsSnapshot {
    pub ticks: u64,
    pub ticks_skipped: u64,
    pub batches_submitted: u64,
    pub inputs_submitted: u64,
    pub inputs_received: u64,
    pub inputs_late: u64,
    pub inputs_unbound: u64,
    pub hellos: u64,
    pub challenges: u64,
    pub auths_accepted: u64,
    pub auths_refused: u64,
    pub sessions_expired: u64,
    pub malformed: u64,
    pub datagrams_sent: u64,
    pub wire_bytes_sent: u64,
    pub states_sent: u64,
    pub send_errors: u64,
    pub send_overruns: u64,
    pub per_thread_datagrams: Vec<u64>,
    /// Sending one tick to every player: from the tick reaching the gateway to its last datagram sent, in ms.
    pub send_ms: Spread,
    /// How old a tick was when it reached the gateway, in ms.
    pub arrival_ms: Spread,
}

impl Stats {
    pub fn new(send_threads: usize) -> Stats {
        Stats { per_thread_datagrams: (0..send_threads).map(|_| AtomicU64::new(0)).collect(), ..Stats::default() }
    }

    pub(crate) fn record_send(&self, micros: u32) {
        let mut t = self.timings.lock().unwrap();
        if t.send_us.len() >= MAX_TIMINGS {
            t.send_us.remove(0);
        }
        t.send_us.push(micros);
        // (nobody may be taking the window: keep it as bounded as the whole)
        if t.send_window_us.len() >= MAX_TIMINGS {
            t.send_window_us.remove(0);
        }
        t.send_window_us.push(micros);
    }

    /// The times to send a tick since the last call (or the start), in ms, and forget them: what
    /// one report window saw, where `snapshot`'s figures run over the whole of the run so far.
    pub fn take_send_window(&self) -> Spread {
        let window = std::mem::take(&mut self.timings.lock().unwrap().send_window_us);
        Spread::of(window.into_iter().map(|v| v as f64 / 1e3).collect())
    }

    pub(crate) fn record_arrival(&self, micros: i64) {
        let mut t = self.timings.lock().unwrap();
        if t.arrival_us.len() >= MAX_TIMINGS {
            t.arrival_us.remove(0);
        }
        t.arrival_us.push(micros);
    }

    pub fn snapshot(&self) -> StatsSnapshot {
        let (send, arrival) = {
            let t = self.timings.lock().unwrap();
            (
                t.send_us.iter().map(|v| *v as f64 / 1e3).collect(),
                t.arrival_us.iter().map(|v| *v as f64 / 1e3).collect(),
            )
        };
        StatsSnapshot {
            ticks: self.ticks.load(Relaxed),
            ticks_skipped: self.ticks_skipped.load(Relaxed),
            batches_submitted: self.batches_submitted.load(Relaxed),
            inputs_submitted: self.inputs_submitted.load(Relaxed),
            inputs_received: self.inputs_received.load(Relaxed),
            inputs_late: self.inputs_late.load(Relaxed),
            inputs_unbound: self.inputs_unbound.load(Relaxed),
            hellos: self.hellos.load(Relaxed),
            challenges: self.challenges.load(Relaxed),
            auths_accepted: self.auths_accepted.load(Relaxed),
            auths_refused: self.auths_refused.load(Relaxed),
            sessions_expired: self.sessions_expired.load(Relaxed),
            malformed: self.malformed.load(Relaxed),
            datagrams_sent: self.datagrams_sent.load(Relaxed),
            wire_bytes_sent: self.wire_bytes_sent.load(Relaxed),
            states_sent: self.states_sent.load(Relaxed),
            send_errors: self.send_errors.load(Relaxed),
            send_overruns: self.send_overruns.load(Relaxed),
            per_thread_datagrams: self.per_thread_datagrams.iter().map(|c| c.load(Relaxed)).collect(),
            send_ms: Spread::of(send),
            arrival_ms: Spread::of(arrival),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_send_window_holds_only_the_ticks_since_it_was_last_taken() {
        let stats = Stats::new(1);
        for us in [1_000, 2_000, 9_000] {
            stats.record_send(us);
        }
        let first = stats.take_send_window();
        assert_eq!((first.p50, first.max), (2.0, 9.0));
        stats.record_send(4_000);
        let second = stats.take_send_window();
        assert_eq!((second.p50, second.max), (4.0, 4.0));
        assert_eq!(stats.take_send_window(), Spread::default());
        // the snapshot still runs over everything
        assert_eq!(stats.snapshot().send_ms.max, 9.0);
    }
}
