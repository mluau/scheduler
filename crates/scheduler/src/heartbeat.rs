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
    shutdown_channel: HeartbeatChan,
}

impl Default for Heartbearter {
    fn default() -> Self {
        Self::new()
    }
}

impl Heartbearter {
    pub fn new() -> Self {
        Self::new_with_duration(Duration::from_millis(TICK_MILLIS))
    }

    pub fn new_with_duration(dur: Duration) -> Self {
        let chan = flume::unbounded();
        let shutdown_channel = flume::unbounded();
        Self {
            tick_duration: dur,
            interval: interval(dur),
            chan,
            shutdown_channel,
        }
    }

    pub fn reciever(&self) -> flume::Receiver<()> {
        self.chan.1.clone()
    }

    pub fn shutdown_sender(&self) -> flume::Sender<()> {
        self.shutdown_channel.0.clone()
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
            tokio::select! {
                _ = self.interval.tick() => {
                    match self.chan.0.send_async(()).await {
                        Ok(_) => {}
                        Err(hb) => {
                            log::warn!("Heartbeat: failed to send tick: {}", hb);
                        }
                    }
                }
                _ = self.shutdown_channel.1.recv_async() => {
                    log::info!("Heartbeat: shutting down");
                    break;
                }
            }
        }
    }
}
