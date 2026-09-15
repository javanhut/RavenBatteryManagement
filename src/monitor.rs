//! The one live picture of the battery the whole window shares: the latest
//! reading, the estimate built on it, and the history it came from. A
//! timer refreshes it every few seconds; every page reads from it.

use crate::history::{self, Estimate, History};
use crate::power::BatteryInfo;

pub struct Monitor {
    pub battery: BatteryInfo,
    pub estimate: Estimate,
    pub history: History,
    /// Unix seconds of the latest refresh.
    pub now: u64,
}

impl Monitor {
    pub fn start() -> Self {
        let battery = BatteryInfo::read();
        let now = history::now();
        let history = if battery.is_real {
            History::load()
        } else {
            History::demo(now)
        };
        let estimate = history.estimate(&battery, now);
        let mut monitor = Self {
            battery,
            estimate,
            history,
            now,
        };
        monitor.record();
        monitor
    }

    /// Read the battery again, re-estimate, and take a sample if it is due.
    pub fn refresh(&mut self) {
        self.battery = BatteryInfo::read();
        self.now = history::now();
        self.estimate = self.history.estimate(&self.battery, self.now);
        self.record();
    }

    fn record(&mut self) {
        if self.battery.is_real {
            self.history.record(&self.battery, &self.estimate, self.now);
        }
    }
}
