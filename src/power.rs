use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// Whether the machine is drawing from the battery, filling it, or sitting
/// on the adapter with the battery full (or held at a charge limit).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerState {
    OnBattery,
    Charging,
    PluggedIn,
}

impl PowerState {
    pub fn label(self) -> &'static str {
        match self {
            PowerState::OnBattery => "On battery",
            PowerState::Charging => "Charging",
            PowerState::PluggedIn => "Plugged in",
        }
    }
}

#[derive(Clone, Debug)]
pub struct BatteryInfo {
    pub percent: u8,
    pub status: String,
    /// `Some(true)` when a mains or USB supply reports itself online.
    pub ac_online: Option<bool>,
    pub power_watts: f64,
    pub energy_now_wh: f64,
    pub energy_full_wh: f64,
    pub energy_design_wh: f64,
    pub temperature_c: Option<f64>,
    pub cycle_count: Option<u32>,
    pub battery_name: String,
    pub is_real: bool,
}

impl Default for BatteryInfo {
    fn default() -> Self {
        Self {
            percent: 90,
            status: "Discharging".into(),
            ac_online: Some(false),
            power_watts: 8.4,
            energy_now_wh: 60.5,
            energy_full_wh: 67.2,
            energy_design_wh: 70.0,
            temperature_c: Some(34.0),
            cycle_count: Some(84),
            battery_name: "Battery".into(),
            is_real: false,
        }
    }
}

impl BatteryInfo {
    pub fn read() -> Self {
        find_battery()
            .and_then(|path| read_battery(&path))
            .unwrap_or_default()
    }

    pub fn health_percent(&self) -> u8 {
        if self.energy_design_wh <= 0.0 {
            return 100;
        }
        ((self.energy_full_wh / self.energy_design_wh) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u8
    }

    /// The kernel's `status` first; the adapter decides the ambiguous cases.
    /// "Not charging" and "Full" both mean the adapter is carrying the
    /// load, whether the battery is at 100% or held at a charge limit.
    pub fn state(&self) -> PowerState {
        match self.status.to_ascii_lowercase().as_str() {
            "charging" => PowerState::Charging,
            "discharging" => PowerState::OnBattery,
            "full" | "not charging" => PowerState::PluggedIn,
            _ => match self.ac_online {
                Some(true) => PowerState::PluggedIn,
                Some(false) => PowerState::OnBattery,
                None if self.power_watts > 0.1 => PowerState::OnBattery,
                None => PowerState::PluggedIn,
            },
        }
    }

    /// Minutes to empty (on battery) or to full (charging) from nothing but
    /// this instant's power reading, the way a naive meter would show it.
    pub fn instant_minutes(&self) -> Option<u32> {
        if self.power_watts <= 0.1 {
            return None;
        }
        let energy = match self.state() {
            PowerState::OnBattery if self.energy_now_wh > 0.0 => self.energy_now_wh,
            PowerState::Charging if self.energy_full_wh > self.energy_now_wh => {
                self.energy_full_wh - self.energy_now_wh
            }
            _ => return None,
        };
        Some((energy / self.power_watts * 60.0).round() as u32)
    }
}

fn supplies_of_type(wanted: &[&str]) -> Vec<PathBuf> {
    fs::read_dir("/sys/class/power_supply")
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|path| {
                    read_text(path.join("type"))
                        .is_some_and(|value| wanted.iter().any(|w| value.eq_ignore_ascii_case(w)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn find_battery() -> Option<PathBuf> {
    supplies_of_type(&["Battery"]).into_iter().next()
}

/// `Some(true)` when any mains or USB supply says it is online, `Some(false)`
/// when they all say they are not, `None` when the kernel exposes none.
fn adapter_online() -> Option<bool> {
    let states: Vec<bool> = supplies_of_type(&["Mains", "USB", "USB_PD", "USB_C"])
        .into_iter()
        .filter_map(|path| read_num(path.join("online")).map(|v| v != 0))
        .collect();
    if states.is_empty() {
        None
    } else {
        Some(states.iter().any(|online| *online))
    }
}

fn read_battery(path: &Path) -> Option<BatteryInfo> {
    let percent = read_num(path.join("capacity"))? as u8;
    let voltage = micro_value(path, &["voltage_now"]).unwrap_or(0.0);
    let energy_now = energy_value(path, "energy_now", "charge_now", voltage).unwrap_or(0.0);
    let energy_full = energy_value(path, "energy_full", "charge_full", voltage).unwrap_or(0.0);
    let energy_design = energy_value(path, "energy_full_design", "charge_full_design", voltage)
        .unwrap_or(energy_full);
    let power = micro_value(path, &["power_now"])
        .or_else(|| {
            let current = micro_value(path, &["current_now"])?;
            let voltage = micro_value(path, &["voltage_now"])?;
            Some(current * voltage)
        })
        .unwrap_or(0.0);
    Some(BatteryInfo {
        percent,
        status: read_text(path.join("status")).unwrap_or_else(|| "Unknown".into()),
        ac_online: adapter_online(),
        power_watts: power,
        energy_now_wh: energy_now,
        energy_full_wh: energy_full,
        energy_design_wh: energy_design,
        temperature_c: read_num(path.join("temp")).map(|v| v as f64 / 10.0),
        cycle_count: read_num(path.join("cycle_count")).map(|v| v as u32),
        battery_name: read_text(path.join("model_name"))
            .unwrap_or_else(|| "Internal battery".into()),
        is_real: true,
    })
}

fn micro_value(path: &Path, names: &[&str]) -> Option<f64> {
    names
        .iter()
        .find_map(|name| read_num(path.join(name)).map(|value| value as f64 / 1_000_000.0))
}

fn energy_value(path: &Path, energy_name: &str, charge_name: &str, voltage: f64) -> Option<f64> {
    micro_value(path, &[energy_name])
        .or_else(|| micro_value(path, &[charge_name]).map(|amp_hours| amp_hours * voltage))
}
fn read_num(path: impl AsRef<Path>) -> Option<u64> {
    read_text(path)?.parse().ok()
}
fn read_text(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_string())
}

#[derive(Clone, Debug)]
pub struct ProcessGroup {
    pub name: String,
    pub pids: Vec<u32>,
    pub cpu_ticks: u64,
    pub memory_mb: f64,
}

pub fn active_processes() -> Vec<ProcessGroup> {
    let mut grouped: HashMap<String, ProcessGroup> = HashMap::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let base = entry.path();
        let Some(name) = read_text(base.join("comm")) else {
            continue;
        };
        let stat = read_text(base.join("stat")).unwrap_or_default();
        let fields: Vec<&str> = stat.split_whitespace().collect();
        let ticks = fields
            .get(13)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
            + fields
                .get(14)
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);
        let memory_kb = read_text(base.join("status"))
            .and_then(|status| {
                status
                    .lines()
                    .find(|line| line.starts_with("VmRSS:"))
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|v| v.parse::<f64>().ok())
            })
            .unwrap_or(0.0);
        let group = grouped.entry(name.clone()).or_insert(ProcessGroup {
            name,
            pids: Vec::new(),
            cpu_ticks: 0,
            memory_mb: 0.0,
        });
        group.pids.push(pid);
        group.cpu_ticks += ticks;
        group.memory_mb += memory_kb / 1024.0;
    }
    let mut values: Vec<_> = grouped
        .into_values()
        .filter(|g| g.memory_mb > 10.0)
        .collect();
    values.sort_by_key(|g| std::cmp::Reverse(g.cpu_ticks));
    values.truncate(12);
    values
}

pub fn set_process_eco(pids: &[u32], eco: bool) -> Result<(), String> {
    let priority = if eco { "10" } else { "0" };
    for pid in pids {
        let output = Command::new("renice")
            .args([priority, "-p", &pid.to_string()])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() && !eco {
            return Err("Restoring normal priority may require authentication".into());
        }
    }
    Ok(())
}

/// raven-powerd's desktop socket. Group `video`, which the session already
/// holds, so profile requests need no authorization prompt at all.
const POWER_SOCKET: &str = "/run/raven-power/ctl";

/// Where raven-powerd publishes the profile it last applied, one word.
const PROFILE_MARKER: &str = "/run/raven-power/profile";

const POWERD_TIMEOUT: Duration = Duration::from_secs(2);

/// One line to raven-powerd, one line back, trimmed.
fn ask_powerd(request: &str) -> Result<String, String> {
    let mut stream = UnixStream::connect(POWER_SOCKET)
        .map_err(|e| format!("raven-powerd is not reachable: {e}"))?;
    stream.set_read_timeout(Some(POWERD_TIMEOUT)).ok();
    stream.set_write_timeout(Some(POWERD_TIMEOUT)).ok();
    stream
        .write_all(format!("{request}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    Ok(reply.trim().to_string())
}

/// The preset named in a raven-powerd reply or marker such as
/// "power-saver (auto)". `None` for "unmanaged" and for errors.
fn profile_from_reply(reply: &str) -> Option<String> {
    let word = reply.split_whitespace().next()?;
    matches!(word, "performance" | "balanced" | "power-saver").then(|| word.to_string())
}

pub fn active_power_profile() -> String {
    // The daemon that actually owns the governor on Raven Linux, first.
    if let Some(profile) = ask_powerd("profile")
        .ok()
        .and_then(|reply| profile_from_reply(&reply))
    {
        return profile;
    }
    if let Some(profile) = fs::read_to_string(PROFILE_MARKER)
        .ok()
        .and_then(|marker| profile_from_reply(&marker))
    {
        return profile;
    }
    if let Some(profile) = Command::new("powerprofilesctl")
        .arg("get")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        return profile;
    }
    let policy = Path::new("/sys/devices/system/cpu/cpufreq/policy0");
    let governor = read_text(policy.join("scaling_governor")).unwrap_or_default();
    if governor == "performance" {
        return "performance".into();
    }
    let current_max = read_num(policy.join("scaling_max_freq")).unwrap_or(0);
    let hardware_max = read_num(policy.join("cpuinfo_max_freq")).unwrap_or(current_max);
    if hardware_max > 0 && current_max * 100 / hardware_max <= 70 {
        "power-saver".into()
    } else {
        "balanced".into()
    }
}

pub fn set_power_profile(profile: &str) -> Result<(), String> {
    validate_profile(profile)?;
    // Raven Linux: raven-powerd owns the governor and re-applies its preset
    // on every supply poll, so a sysfs write behind its back would not stick.
    // Its socket takes the request directly, with no authorization prompt.
    match ask_powerd(&format!("profile {profile}")) {
        Ok(reply) if !reply.starts_with("error") => return Ok(()),
        Ok(reply) if !reply.contains("unknown command") => {
            return Err(format!("raven-powerd refused: {reply}"));
        }
        // A daemon too old to know the verb, or no daemon at all: fall
        // through to the paths every other system uses.
        _ => {}
    }
    if command_exists("powerprofilesctl") {
        let status = Command::new("powerprofilesctl")
            .args(["set", profile])
            .status()
            .map_err(|e| format!("Could not start the system profile service: {e}"))?;
        return if status.success() {
            Ok(())
        } else {
            Err("The system power-profile service rejected this change".into())
        };
    }
    match apply_profile_sysfs(profile) {
        Ok(()) => return Ok(()),
        Err(error) if !is_permission_error(&error) => return Err(error),
        Err(_) => {}
    }
    let executable = std::env::current_exe()
        .map_err(|e| format!("Could not locate the Raven Power executable: {e}"))?;
    // run0 needs a booted systemd; without one it fails before any
    // authorization happens, which must not be reported as a denial.
    let systemd_booted = Path::new("/run/systemd/system").is_dir();
    let output = if command_exists("run0") && systemd_booted {
        Command::new("run0")
            .args([
                "--unit=raven-power-profile",
                "--description=Apply Raven power profile",
            ])
            .arg(&executable)
            .args(["--apply-profile", profile])
            .output()
    } else if command_exists("pkexec") {
        Command::new("pkexec")
            .arg(&executable)
            .args(["--apply-profile", profile])
            .output()
    } else {
        return Err(
            "Raven Power needs raven-powerd, run0, or pkexec to change kernel power settings"
                .into(),
        );
    }
    .map_err(|e| format!("Could not request authorization: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        if stderr.is_empty() {
            Err("The profile was not changed. Authorization was cancelled or denied.".into())
        } else {
            Err(format!("The profile was not changed: {stderr}"))
        }
    }
}

pub fn apply_profile_sysfs(profile: &str) -> Result<(), String> {
    validate_profile(profile)?;
    let root = Path::new("/sys/devices/system/cpu/cpufreq");
    let entries = fs::read_dir(root)
        .map_err(|_| "This system does not expose CPU frequency controls".to_string())?;
    let mut policies = Vec::new();
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("policy") {
            policies.push(entry.path());
        }
    }
    if policies.is_empty() {
        return Err("This CPU driver does not expose any controllable policies".into());
    }
    for policy in policies {
        apply_policy(&policy, profile)?;
    }
    Ok(())
}

fn apply_policy(policy: &Path, profile: &str) -> Result<(), String> {
    let available = read_text(policy.join("scaling_available_governors")).unwrap_or_default();
    let governor = match profile {
        "performance" if available.split_whitespace().any(|v| v == "performance") => "performance",
        "power-saver" if available.split_whitespace().any(|v| v == "powersave") => "powersave",
        "power-saver" if available.split_whitespace().any(|v| v == "ondemand") => "ondemand",
        _ if available.split_whitespace().any(|v| v == "schedutil") => "schedutil",
        _ if available.split_whitespace().any(|v| v == "ondemand") => "ondemand",
        _ => {
            return Err(format!(
                "No suitable CPU governor is available for {profile}"
            ));
        }
    };
    write_control(policy.join("scaling_governor"), governor)?;

    let hardware_max = read_num(policy.join("cpuinfo_max_freq"));
    if let Some(hardware_max) = hardware_max {
        let target = if profile == "power-saver" {
            hardware_max * 60 / 100
        } else {
            hardware_max
        };
        if policy.join("scaling_max_freq").exists() {
            write_control(policy.join("scaling_max_freq"), &target.to_string())?;
        }
    }
    let epp = policy.join("energy_performance_preference");
    if epp.exists() {
        let available =
            read_text(policy.join("energy_performance_available_preferences")).unwrap_or_default();
        let preferred = match profile {
            "performance" => "performance",
            "power-saver" => "power",
            _ => "balance_performance",
        };
        if available.split_whitespace().any(|value| value == preferred) {
            write_control(epp, preferred)?;
        }
    }
    Ok(())
}

fn write_control(path: PathBuf, value: &str) -> Result<(), String> {
    fs::write(&path, value).map_err(|error| format!("{}: {error}", path.display()))
}

fn validate_profile(profile: &str) -> Result<(), String> {
    if matches!(profile, "performance" | "balanced" | "power-saver") {
        Ok(())
    } else {
        Err(format!("Invalid power profile: {profile}"))
    }
}

fn command_exists(command: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|path| path.join(command).is_file()))
}

fn is_permission_error(error: &str) -> bool {
    error.contains("Permission denied") || error.contains("Read-only file system")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub profile: String,
    pub charge_limit: u8,
    pub cpu_limit: u8,
    pub brightness_limit: u8,
    pub background_saving: bool,
    pub wifi_saving: bool,
    pub eco_apps: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            profile: "balanced".into(),
            charge_limit: 80,
            cpu_limit: 75,
            brightness_limit: 80,
            background_saving: true,
            wifi_saving: true,
            eco_apps: Vec::new(),
        }
    }
}

impl Settings {
    pub fn load() -> Self {
        fs::read_to_string(config_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) {
        let path = config_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, data);
        }
    }
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("raven-power/settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_battery_health() {
        let battery = BatteryInfo {
            energy_full_wh: 63.0,
            energy_design_wh: 70.0,
            ..Default::default()
        };
        assert_eq!(battery.health_percent(), 90);
    }

    #[test]
    fn instant_estimate_uses_the_energy_left() {
        let battery = BatteryInfo {
            status: "Discharging".into(),
            energy_now_wh: 36.0,
            power_watts: 8.0,
            ..Default::default()
        };
        assert_eq!(battery.state(), PowerState::OnBattery);
        assert_eq!(battery.instant_minutes(), Some(270));
    }

    #[test]
    fn instant_estimate_counts_to_full_while_charging() {
        let battery = BatteryInfo {
            status: "Charging".into(),
            energy_now_wh: 40.0,
            energy_full_wh: 70.0,
            power_watts: 30.0,
            ..Default::default()
        };
        assert_eq!(battery.state(), PowerState::Charging);
        assert_eq!(battery.instant_minutes(), Some(60));
    }

    #[test]
    fn adapter_decides_ambiguous_statuses() {
        let full = BatteryInfo {
            status: "Full".into(),
            ..Default::default()
        };
        assert_eq!(full.state(), PowerState::PluggedIn);
        let held = BatteryInfo {
            status: "Not charging".into(),
            percent: 80,
            ..Default::default()
        };
        assert_eq!(held.state(), PowerState::PluggedIn);
        assert_eq!(held.instant_minutes(), None);
        let unknown_on_ac = BatteryInfo {
            status: "Unknown".into(),
            ac_online: Some(true),
            ..Default::default()
        };
        assert_eq!(unknown_on_ac.state(), PowerState::PluggedIn);
        let unknown_no_ac = BatteryInfo {
            status: "Unknown".into(),
            ac_online: Some(false),
            ..Default::default()
        };
        assert_eq!(unknown_no_ac.state(), PowerState::OnBattery);
    }

    #[test]
    fn settings_round_trip_json() {
        let settings = Settings {
            profile: "power-saver".into(),
            charge_limit: 75,
            ..Default::default()
        };
        let encoded = serde_json::to_string(&settings).unwrap();
        let decoded: Settings = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.profile, "power-saver");
        assert_eq!(decoded.charge_limit, 75);
    }

    #[test]
    fn privileged_helper_rejects_unknown_profiles() {
        let error = apply_profile_sysfs("arbitrary-value").unwrap_err();
        assert!(error.contains("Invalid power profile"));
    }

    #[test]
    fn parses_powerd_profile_replies() {
        assert_eq!(
            profile_from_reply("power-saver (override)").as_deref(),
            Some("power-saver")
        );
        assert_eq!(
            profile_from_reply("balanced (auto)").as_deref(),
            Some("balanced")
        );
        assert_eq!(
            profile_from_reply("performance").as_deref(),
            Some("performance")
        );
        assert_eq!(profile_from_reply("unmanaged"), None);
        assert_eq!(profile_from_reply("error: unknown command"), None);
        assert_eq!(profile_from_reply(""), None);
    }
}
