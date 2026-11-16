//! What the Environment panel says, apart from how it draws it: the VM's and
//! the sandbox's state, a diagnostics reading as a headline, meters and
//! lists, a command's outcome. It names no UI type, so its tests run without
//! the app.
use serde::Deserialize;
use serde_json::Value;

/// One part of the panel that runs things in the VM, each with its own
/// progress and failure.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Section {
    Vm,
    Diagnostics,
    Screen,
    Command,
}

impl Section {
    /// The section an engine action (`ControlProjectWindowsVm`) reports to.
    pub(super) fn of(action: &str) -> Self {
        match action {
            "diagnostics" => Self::Diagnostics,
            "screenshot" => Self::Screen,
            "exec" | "launch" => Self::Command,
            _ => Self::Vm,
        }
    }
}

/// How a state reads at a glance: the colour of its dot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Tone {
    Good,
    Busy,
    Off,
    Bad,
}

/// What the rail's entry says the project uses, if anything.
pub(super) fn label(sandbox: bool, windows_vm: bool) -> Option<&'static str> {
    match (sandbox, windows_vm) {
        (true, true) => Some("Environment: sandbox and Windows VM"),
        (true, false) => Some("Environment: sandbox"),
        (false, true) => Some("Environment: Windows VM"),
        (false, false) => None,
    }
}

/// The VM's state as its pill says it. `state` is `winvm status`'s.
pub(super) fn vm_state(connected: bool, state: &str) -> (Tone, &'static str) {
    if !connected {
        return (Tone::Off, "Not connected");
    }
    match state {
        "running" => (Tone::Good, "Running"),
        "starting" => (Tone::Busy, "Starting"),
        "stopped" => (Tone::Off, "Stopped"),
        // A brief reading has no state yet.
        "" => (Tone::Off, "Checking"),
        _ => (Tone::Bad, "Unavailable"),
    }
}

/// The sandbox's state as its pill says it. `state` is podman's
/// (`running`, `exited`, `not created`, …).
pub(super) fn sandbox_state(state: &str, building: bool) -> (Tone, String) {
    if building {
        return (Tone::Busy, "Building".into());
    }
    let mut letters = state.chars();
    let text = letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_else(|| "Unknown".to_string());
    let tone = match state {
        "running" => Tone::Good,
        "created" | "configured" | "initialized" => Tone::Busy,
        _ => Tone::Off,
    };
    (tone, text)
}

/// How full a meter is, in the colours the usage meters use: fine, worth a
/// look from four fifths, critical from 95%.
pub(super) fn level(fraction: f32) -> Tone {
    if fraction >= 0.95 {
        Tone::Bad
    } else if fraction >= 0.8 {
        Tone::Busy
    } else {
        Tone::Good
    }
}

/// A share of something the VM has: what, how much is taken (0 to 1), and
/// the amount in words.
#[derive(Clone, PartialEq, Debug)]
pub(super) struct Meter {
    pub(super) label: &'static str,
    pub(super) fraction: f32,
    pub(super) value: String,
}

/// PowerShell writes a value it doesn't have as `null`; here that is the
/// same as leaving it out.
fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(members) => Value::Object(
            members
                .into_iter()
                .filter(|(_, value)| !value.is_null())
                .map(|(name, value)| (name, without_nulls(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(without_nulls).collect()),
        value => value,
    }
}

#[derive(Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default)]
pub(super) struct Amount {
    total: f64,
    free: f64,
}

#[derive(Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Process {
    name: String,
    pid: u64,
    #[serde(rename = "memoryMB")]
    memory_mb: f64,
}

/// `winvm diag`: the running guest's health in one reading.
#[derive(Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Diagnostics {
    os: String,
    computer_name: String,
    uptime_seconds: u64,
    logged_on_user: String,
    cpu_load_percent: f64,
    #[serde(rename = "memoryMB")]
    memory_mb: Amount,
    #[serde(rename = "diskGB")]
    disk_gb: Amount,
    /// `WIDTHxHEIGHT@HZ adapter`.
    pub(super) display: String,
    top_processes: Vec<Process>,
    /// `address:port process`, one per listener.
    pub(super) listening_ports: Vec<String>,
    pub(super) stopped_automatic_services: Vec<String>,
    /// `HH:MM:SS source id: message`, the last hour's.
    pub(super) recent_errors: Vec<String>,
}

impl Diagnostics {
    /// The engine's `result` for the `diagnostics` action.
    pub(super) fn read(result: Value) -> Result<Self, String> {
        let reading: Self = serde_json::from_value(without_nulls(result))
            .map_err(|error| format!("The VM's reading could not be read: {error}"))?;
        if reading.os.is_empty() {
            return Err("The VM answered without a reading.".into());
        }
        Ok(reading)
    }

    /// Who and what the machine is, most telling first: Windows' edition,
    /// how long it has been up, who is signed in.
    pub(super) fn headline(&self) -> Vec<String> {
        let edition = self.os.strip_prefix("Microsoft ").unwrap_or(&self.os);
        let edition = edition.split(" (build").next().unwrap_or(edition);
        let user = if self.logged_on_user.is_empty() {
            "nobody signed in".to_string()
        } else {
            format!("{} signed in", self.logged_on_user)
        };
        vec![
            edition.to_string(),
            format!("up {}", duration(self.uptime_seconds)),
            user,
        ]
    }

    /// The processor, memory and disk C:, each as how much is taken.
    pub(super) fn meters(&self) -> [Meter; 3] {
        let taken = |label, amount: &Amount, per_gb: f64| {
            let (total, used) = (amount.total / per_gb, (amount.total - amount.free) / per_gb);
            Meter {
                label,
                fraction: if total > 0.0 {
                    (used / total).clamp(0.0, 1.0) as f32
                } else {
                    0.0
                },
                value: if total > 0.0 {
                    format!("{used:.1} of {total:.1} GB")
                } else {
                    "Unknown".into()
                },
            }
        };
        [
            Meter {
                label: "CPU",
                fraction: (self.cpu_load_percent / 100.0).clamp(0.0, 1.0) as f32,
                value: format!("{:.0}%", self.cpu_load_percent),
            },
            taken("Memory", &self.memory_mb, 1024.0),
            taken("Disk C:", &self.disk_gb, 1.0),
        ]
    }

    /// The processes holding the most memory: name with pid, and memory.
    pub(super) fn processes(&self) -> Vec<(String, String)> {
        self.top_processes
            .iter()
            .map(|process| {
                (
                    format!("{} · pid {}", process.name, process.pid),
                    format!("{:.0} MB", process.memory_mb),
                )
            })
            .collect()
    }
}

/// How long ago a reading was taken, for the line under it.
pub(super) fn ago(seconds: u64) -> String {
    match seconds {
        0..10 => "just now".into(),
        10..60 => format!("{seconds} s ago"),
        _ => format!("{} ago", duration(seconds)),
    }
}

/// A length of time in its two largest units: `2 h 13 min`.
pub(super) fn duration(seconds: u64) -> String {
    let (days, hours, minutes) = (seconds / 86_400, seconds / 3_600 % 24, seconds / 60 % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "less than a minute".into(),
        (0, 0, minutes) => format!("{minutes} min"),
        (0, hours, minutes) => format!("{hours} h {minutes} min"),
        (days, hours, _) => format!("{days} d {hours} h"),
    }
}

/// A PowerShell command that ran in the VM (the `exec` action's `result`).
#[derive(Deserialize, Clone, Default, PartialEq, Debug)]
#[serde(default, rename_all = "camelCase")]
pub(super) struct Ran {
    exit_code: Option<i64>,
    timed_out: bool,
    stdout: String,
    stderr: String,
}

impl Ran {
    pub(super) fn read(result: Value) -> Result<Self, String> {
        serde_json::from_value(without_nulls(result))
            .map_err(|error| format!("The command's result could not be read: {error}"))
    }

    /// How it ended.
    pub(super) fn outcome(&self) -> (Tone, String) {
        match self.exit_code {
            _ if self.timed_out => (Tone::Bad, "Stopped · it ran longer than two minutes".into()),
            Some(0) => (Tone::Good, "Finished · exit code 0".into()),
            Some(code) => (Tone::Bad, format!("Failed · exit code {code}")),
            None => (Tone::Bad, "Ended without an exit code".into()),
        }
    }

    /// What it printed: standard output, then standard error.
    pub(super) fn output(&self) -> String {
        let printed = [self.stdout.trim_end(), self.stderr.trim_end()]
            .into_iter()
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if printed.is_empty() {
            "It printed nothing.".into()
        } else {
            printed
        }
    }
}

/// A PNG's width and height in pixels, from its header.
pub(super) fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    let number = |at: usize| Some(u32::from_be_bytes(png.get(at..at + 4)?.try_into().ok()?));
    let size = (number(16)?, number(20)?);
    (png.starts_with(b"\x89PNG\r\n\x1a\n") && size.0 > 0 && size.1 > 0).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_screenshots_size_comes_from_its_header() {
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend([0, 0, 0x03, 0xb0, 0, 0, 0x04, 0]);
        assert_eq!(png_size(&png), Some((944, 1024)));
        assert_eq!(png_size(&png[..20]), None);
        assert_eq!(png_size(b"PNG bytes, but not a picture"), None);
        assert_eq!(png_size(b""), None);
    }

    #[test]
    fn the_entry_names_what_is_in_use_and_nothing_otherwise() {
        assert_eq!(label(false, false), None);
        assert_eq!(label(true, false), Some("Environment: sandbox"));
        assert!(label(true, true).unwrap().contains("Windows VM"));
    }

    #[test]
    fn every_engine_action_reports_to_the_part_that_started_it() {
        for (action, section) in [
            ("start", Section::Vm),
            ("stop", Section::Vm),
            ("view", Section::Vm),
            ("diagnostics", Section::Diagnostics),
            ("screenshot", Section::Screen),
            ("exec", Section::Command),
            ("launch", Section::Command),
        ] {
            assert_eq!(Section::of(action), section, "{action}");
        }
    }

    #[test]
    fn the_pills_say_what_the_vm_and_the_container_are_doing() {
        assert_eq!(vm_state(true, "running"), (Tone::Good, "Running"));
        assert_eq!(vm_state(true, "starting"), (Tone::Busy, "Starting"));
        assert_eq!(vm_state(true, "stopped"), (Tone::Off, "Stopped"));
        assert_eq!(vm_state(true, "unavailable"), (Tone::Bad, "Unavailable"));
        assert_eq!(vm_state(true, ""), (Tone::Off, "Checking"));
        // A project that isn't connected says so whatever the VM does.
        assert_eq!(vm_state(false, "running"), (Tone::Off, "Not connected"));

        assert_eq!(sandbox_state("running", false), (Tone::Good, "Running".into()));
        assert_eq!(sandbox_state("exited", false), (Tone::Off, "Exited".into()));
        assert_eq!(sandbox_state("not created", false).1, "Not created");
        assert_eq!(sandbox_state("", false).1, "Unknown");
        // A build says so over whatever the old container does.
        assert_eq!(sandbox_state("running", true), (Tone::Busy, "Building".into()));
    }

    #[test]
    fn meters_warn_from_four_fifths_and_are_critical_from_95_percent() {
        assert_eq!(level(0.0), Tone::Good);
        assert_eq!(level(0.79), Tone::Good);
        assert_eq!(level(0.8), Tone::Busy);
        assert_eq!(level(0.94), Tone::Busy);
        assert_eq!(level(0.95), Tone::Bad);
        assert_eq!(level(1.0), Tone::Bad);
    }

    #[test]
    fn a_real_reading_becomes_a_headline_meters_and_lists() {
        let reading = Diagnostics::read(
            serde_json::from_str(include_str!("fixtures/winvm-diag.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            reading.headline(),
            [
                "Windows 11 Enterprise Evaluation",
                "up 10 min",
                "WINVM\\dev signed in"
            ]
        );
        let [cpu, memory, disk] = reading.meters();
        assert_eq!((cpu.label, cpu.value.as_str()), ("CPU", "4%"));
        assert!((cpu.fraction - 0.04).abs() < 1e-6);
        // 8185 MB with 5460 free; 94.8 GB with 40.2 free.
        assert_eq!(memory.value, "2.7 of 8.0 GB");
        assert!((memory.fraction - 0.3329).abs() < 1e-3);
        assert_eq!(disk.value, "54.6 of 94.8 GB");
        assert!((disk.fraction - 0.5759).abs() < 1e-3);
        assert_eq!(reading.display, "944x1024@60 Red Hat VirtIO GPU DOD controller");
        assert_eq!(
            reading.processes()[0],
            ("MsMpEng · pid 3712".to_string(), "267 MB".to_string())
        );
        assert_eq!(reading.processes().len(), 3);
        assert_eq!(reading.listening_ports[1], "0.0.0.0:22 sshd");
        assert!(reading
            .stopped_automatic_services
            .contains(&"QEMU-GA".to_string()));
        assert!(reading.recent_errors[0].starts_with("11:35:05 "));
    }

    #[test]
    fn a_reading_with_gaps_is_still_shown_and_an_empty_one_is_an_error() {
        // Nobody signed in, no errors in the hour, counters Windows left out.
        let reading = Diagnostics::read(json!({
            "os": "Windows 11", "computerName": "WINVM", "loggedOnUser": null,
            "cpuLoadPercent": null, "recentErrors": null, "topProcesses": [],
        }))
        .unwrap();
        assert_eq!(reading.headline()[0], "Windows 11");
        assert_eq!(reading.headline()[2], "nobody signed in");
        let [cpu, memory, _] = reading.meters();
        assert_eq!((cpu.fraction, cpu.value.as_str()), (0.0, "0%"));
        assert_eq!((memory.fraction, memory.value.as_str()), (0.0, "Unknown"));
        assert!(reading.recent_errors.is_empty() && reading.processes().is_empty());
        assert!(Diagnostics::read(json!({})).is_err());
        assert!(Diagnostics::read(Value::Null).is_err());
        assert!(Diagnostics::read(json!({"os": 11})).is_err());
        // A machine reporting more in use than it has still draws a full bar.
        let odd = Diagnostics::read(json!({"os": "W", "memoryMB": {"total": 100, "free": -50}}));
        assert_eq!(odd.unwrap().meters()[1].fraction, 1.0);
    }

    #[test]
    fn time_reads_in_its_two_largest_units() {
        assert_eq!(duration(0), "less than a minute");
        assert_eq!(duration(59), "less than a minute");
        assert_eq!(duration(608), "10 min");
        assert_eq!(duration(3 * 3600 + 13 * 60 + 5), "3 h 13 min");
        assert_eq!(duration(3600), "1 h 0 min");
        assert_eq!(duration(2 * 86_400 + 5 * 3600 + 59 * 60), "2 d 5 h");
        assert_eq!(ago(3), "just now");
        assert_eq!(ago(42), "42 s ago");
        assert_eq!(ago(125), "2 min ago");
    }

    #[test]
    fn a_commands_result_says_how_it_ended_and_what_it_printed() {
        let ran = |value| Ran::read(value).unwrap();
        let ok = ran(json!({"exitCode": 0, "timedOut": false, "stdout": "WINVM\r\n", "stderr": ""}));
        assert_eq!(ok.outcome(), (Tone::Good, "Finished · exit code 0".into()));
        assert_eq!(ok.output(), "WINVM");
        let failed = ran(json!({"exitCode": 7, "stdout": "partly\n", "stderr": "no such file\n"}));
        assert_eq!(failed.outcome(), (Tone::Bad, "Failed · exit code 7".into()));
        assert_eq!(failed.output(), "partly\nno such file");
        let slow = ran(json!({"exitCode": 124, "timedOut": true, "stdout": "", "stderr": ""}));
        assert!(slow.outcome().1.starts_with("Stopped"));
        assert_eq!(slow.output(), "It printed nothing.");
        assert_eq!(ran(json!({"exitCode": null})).outcome().0, Tone::Bad);
        assert!(Ran::read(json!({"exitCode": "zero"})).is_err());
    }
}
