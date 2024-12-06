use std::time::Duration;
use tokio::time::{interval, Interval};

/// Number of milliseconds to wait before firing a Heartbeat event
pub const TICK_MILLIS: u64 = 1;

pub fn ticks_to_secs(ticks: f64) -> f64 {
    (ticks * TICK_MILLIS as f64) / 1000.0
}

pub fn secs_to_ticks(secs: f64) -> u64 {
    /*println!(
        "{} secs = {} ticks",
        secs,
        ((secs * 1000.0) / TICK_MILLIS as f64) as u64
    );*/
    ((secs * 1000.0) / TICK_MILLIS as f64) as u64
}

pub type HeartbeatChan = (flume::Sender<()>, flume::Receiver<()>);

pub struct Heartbearter {
    tick_duration: std::time::Duration,
    interval: Interval,
    chan: HeartbeatChan,
}

impl Default for Heartbearter {
    fn default() -> Self {
        Self::new()
    }
}

impl Heartbearter {
    pub fn new() -> Self {
        let chan = flume::unbounded();
        Self {
            tick_duration: Duration::from_millis(TICK_MILLIS),
            interval: interval(Duration::from_millis(TICK_MILLIS)),
            chan,
        }
    }

    pub fn reciever(&self) -> flume::Receiver<()> {
        self.chan.1.clone()
    }

    pub fn tick_duration(&self) -> Duration {
        self.tick_duration
    }

    /// Returns the next tick
    pub async fn next_tick(&self) -> Result<(), flume::RecvError> {
        self.chan.1.recv_async().await
    }

    pub async fn run(&mut self) {
        loop {
            self.interval.tick().await;
            match self.chan.0.send_async(()).await {
                Ok(_) => {}
                Err(hb) => {
                    log::warn!("Heartbeat: failed to send tick: {}", hb);
                }
            }
        }
    }
}
