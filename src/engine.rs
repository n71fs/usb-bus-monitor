//! Shared monitoring engine for the desktop and headless interfaces.
use crate::ids::IdDb;
use crate::issues::Watcher;
use crate::klog::KernelLog;
use crate::rates::Rates;
use crate::sysfs::{self, Topology};
use crate::traffic::{Monitor, State};
use std::path::PathBuf;
use std::time::{Duration, Instant};
const RESCAN_EVERY: Duration = Duration::from_secs(2);
const SAMPLE_EVERY: Duration = Duration::from_secs(1);
pub const TICK: Duration = Duration::from_millis(200);
/// Scanning, sampling and issue checks. These run on their own thread, so
/// monitoring and the history carry on while the window is minimized or the
/// screen is locked and nothing is being drawn.
pub struct Engine {
    sys_root: PathBuf,
    ids: IdDb,
    pub topo: Topology,
    pub error: Option<String>,
    last_scan: Instant,
    last_sample: Instant,
    pub monitor: Monitor,
    pub rates: Rates,
    pub watcher: Watcher,
    pub klog: KernelLog,
    /// Bumped on every change, so the window only copies new data.
    pub generation: u64,
}

impl Engine {
    pub fn new(sys_root: PathBuf, ids: IdDb, demo: bool) -> Self {
        let topo = sysfs::scan(&sys_root, &ids).unwrap_or_default();
        let monitor = if demo {
            demo_monitor(&topo)
        } else {
            Monitor::start()
        };
        let klog = if demo {
            KernelLog::default()
        } else {
            KernelLog::start()
        };
        let mut engine = Engine {
            sys_root,
            ids,
            topo,
            error: None,
            last_scan: Instant::now(),
            last_sample: Instant::now(),
            monitor: monitor.clone(),
            rates: Rates::default(),
            watcher: Watcher::new(),
            klog: klog.clone(),
            generation: 0,
        };
        engine.rescan();
        engine
    }

    pub fn rescan(&mut self) {
        match sysfs::scan(&self.sys_root, &self.ids) {
            Ok(t) => {
                self.watcher.on_scan(&t);
                self.topo = t;
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!(
                    "Could not read {}: {e}",
                    self.sys_root.join("bus/usb/devices").display()
                ))
            }
        }
        self.last_scan = Instant::now();
        self.generation += 1;
    }

    pub fn tick(&mut self) {
        if self.last_scan.elapsed() >= RESCAN_EVERY {
            self.rescan();
        }
        if self.last_sample.elapsed() >= SAMPLE_EVERY {
            let live = self.monitor.state() == State::Running;
            self.rates.sample(&self.topo, self.monitor.snapshot());
            self.watcher.on_kernel_lines(self.klog.drain(), &self.topo);
            self.watcher
                .on_sample(&self.topo, &self.rates, &self.rates.latest, live);
            self.last_sample = Instant::now();
            self.generation += 1;
        }
    }
}

/// Plausible SDR-ish rates for every device, for screenshots.
fn demo_monitor(topo: &Topology) -> Monitor {
    let mut all = topo.all_devices();
    all.sort_by_key(|d| d.sdr.is_none());
    let devs = all
        .iter()
        .filter(|d| !d.sysname.starts_with("usb"))
        .map(|d| {
            let rate = match d.sdr {
                Some("RTL-SDR") => 4_800_000,
                Some("Airspy") => 20_000_000,
                Some(_) => 40_000_000,
                None => 2_000,
            };
            (d.busnum as u16, d.devnum as u8, rate)
        })
        .collect();
    Monitor::demo(devs)
}
