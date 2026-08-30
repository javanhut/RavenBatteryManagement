use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug)]
pub struct BatteryInfo {
    pub percent: u8,
    pub status: String,
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

    pub fn remaining_text(&self) -> String {
        if self.status.eq_ignore_ascii_case("charging") {
            return "Charging".into();
        }
        if self.power_watts <= 0.1 {
            return "Calculating…".into();
        }
        let minutes = (self.energy_now_wh / self.power_watts * 60.0).round() as u32;
        format!("{} hr {:02} min", minutes / 60, minutes % 60)
    }
}

fn find_battery() -> Option<PathBuf> {
    fs::read_dir("/sys/class/power_supply")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|path| {
            read_text(path.join("type")).is_some_and(|value| value.eq_ignore_ascii_case("battery"))
        })
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
    values.sort_by(|a, b| b.cpu_ticks.cmp(&a.cpu_ticks));
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

pub fn active_power_profile() -> String {
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
    let status = if command_exists("run0") {
        Command::new("run0")
            .args([
                "--unit=raven-power-profile",
                "--description=Apply Raven power profile",
            ])
            .arg(&executable)
            .args(["--apply-profile", profile])
            .status()
    } else if command_exists("pkexec") {
        Command::new("pkexec")
            .arg(&executable)
            .args(["--apply-profile", profile])
            .status()
    } else {
        return Err("Raven Power needs run0 or pkexec to change kernel power settings".into());
    }
    .map_err(|e| format!("Could not request authorization: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err("The profile was not changed. Authorization was cancelled or denied.".into())
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
    fn estimates_remaining_runtime() {
        let battery = BatteryInfo {
            status: "Discharging".into(),
            energy_now_wh: 36.0,
            power_watts: 8.0,
            ..Default::default()
        };
        assert_eq!(battery.remaining_text(), "4 hr 30 min");
    }

    #[test]
    fn reports_charging_instead_of_a_runtime() {
        let battery = BatteryInfo {
            status: "Charging".into(),
            ..Default::default()
        };
        assert_eq!(battery.remaining_text(), "Charging");
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
}
