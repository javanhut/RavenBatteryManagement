# Raven Power

Native battery management for Raven Linux, written in Rust with GTK 4 and
libadwaita. There is no JavaScript or embedded browser runtime.

## Features

- Live battery telemetry from `/sys/class/power_supply`, refreshed every
  20 seconds, with the adapter state telling on-battery, charging, and
  plugged-in apart
- Runtime and time-to-full estimates from the drain actually observed this
  session (age-weighted, so a sustained change in draw is adopted within
  minutes), shaped by the drain measured at each 10% band on earlier sessions
- A battery history page: charge over time with the projection ahead,
  drain by charge level, and every past prediction scored against what the
  battery went on to do
- One sample a minute while the app is open, kept for 30 days in
  `$XDG_DATA_HOME/raven-power/history.jsonl`
- Linux power-profile switching through `powerprofilesctl`
- Application Eco mode using lower CPU scheduling priority
- Process resource diagnostics from `/proc`
- Granular CPU, brightness, wireless, and charge-limit preferences
- Persistent configuration in `$XDG_CONFIG_HOME/raven-power/settings.json`
- Explicit demo-data state on computers without a battery
- The shared Raven Settings/Store glass shell, palette, accent, and responsive sidebar

## Build and run

The system needs Rust, GTK 4, and libadwaita. `power-profiles-daemon` is
supported when installed, but is not required.

```bash
cargo run
```

For a production build:

```bash
cargo build --release
```

Install the resulting `target/release/raven-power` binary and
`data/org.raven.Power.desktop` through Raven Linux's package build.

## Privileged integration

On Raven Linux, profile switching asks `raven-powerd` over its desktop socket
at `/run/raven-power/ctl` (`profile <preset>`); the session already holds the
`video` group that socket requires, so no authorization prompt is involved and
the daemon that owns the governor stays the only writer. Elsewhere, switching
first uses `powerprofilesctl` when available. Otherwise, Raven Power detects
the kernel CPU-frequency driver and applies validated governor,
maximum-frequency, and energy-performance settings directly. The same binary
exposes a narrowly scoped `--apply-profile` helper mode and requests
authorization through systemd `run0` (only on systems booted with systemd) or
PolicyKit `pkexec`; the graphical UI never runs as root. Charge limits remain
saved policy preferences because their sysfs interfaces are vendor-specific.
