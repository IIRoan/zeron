//! Measuring groups of processes from /proc and cgroups. std and libc only,
//! so it tests apart from the app.
use std::{
    collections::HashMap,
    path::PathBuf,
    time::{Duration, Instant},
};

/// What to measure as one: the processes carrying one of `origins` as their
/// origin marker, the processes `pids`, and everything under them that
/// carries no marker of its own.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Group {
    pub name: String,
    /// Another chat's too: kept out of the totals.
    pub shared: bool,
    pub origins: Vec<String>,
    pub pids: Vec<u32>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Amount {
    /// Percent of one core over the window.
    pub cpu: f64,
    /// Bytes: proportional for a process (resident where the kernel won't
    /// say), `podman stats`' for a container.
    pub memory: u64,
    /// Bytes a second, at the storage layer.
    pub read: f64,
    pub written: f64,
    pub processes: u32,
}

impl Amount {
    fn add(&mut self, other: &Amount) {
        self.cpu += other.cpu;
        self.memory += other.memory;
        self.read += other.read;
        self.written += other.written;
        self.processes += other.processes;
    }
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Usage {
    /// The groups that aren't shared.
    pub total: Amount,
    pub threads: u32,
    pub cores: u32,
    /// Each group's, shared ones too, in the order asked; one with nothing
    /// running is left out, but the first (the chat's own).
    pub groups: Vec<(Group, Amount)>,
    /// Its own biggest holders of memory, by program or container name.
    pub top: Vec<Named>,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Named {
    pub name: String,
    pub count: u32,
    pub amount: Amount,
}

const TOP: usize = 4;

/// `start` tells a process from a later one given the same pid.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    parent: u32,
    start: u64,
    name: String,
    /// Clock ticks on a CPU so far, in user and kernel mode.
    ticks: u64,
    threads: u32,
}

/// `(name, row)` from a `/proc/<pid>/stat` line. The name sits in brackets
/// and may hold spaces and brackets of its own: the last `)` ends it.
/// Zombies (nothing left but an exit status) are left out.
fn parse_stat(stat: &str) -> Option<Row> {
    let (open, close) = (stat.find('(')?, stat.rfind(')')?);
    // (What follows the name starts at field 3, the state.)
    let rest: Vec<&str> = stat.get(close + 2..)?.split_ascii_whitespace().collect();
    if rest.first() == Some(&"Z") {
        return None;
    }
    let field = |n: usize| rest.get(n - 3)?.parse::<u64>().ok();
    Some(Row {
        parent: field(4)? as u32,
        start: field(22)?,
        name: stat.get(open + 1..close)?.to_owned(),
        ticks: field(14)? + field(15)?,
        threads: field(20)? as u32,
    })
}

/// The origin marker in a process's environment (`ZERON_ORIGIN=chat:<id>`).
fn marker(environment: &[u8]) -> Option<&[u8]> {
    environment
        .split(|b| *b == 0)
        .find_map(|entry| entry.strip_prefix(b"ZERON_ORIGIN="))
        .filter(|origin| !origin.is_empty())
}

/// Each process's group (an index into `groups`): its own marker's, or its
/// own pid's, else its nearest ancestor's that has either. A marker no
/// group asked for is someone else's: nothing under it is ours.
fn attribute(table: &HashMap<u32, Row>, groups: &[Group], mut environment: impl FnMut(u32) -> Option<Vec<u8>>) -> HashMap<u32, usize> {
    let mut by_origin: HashMap<&[u8], usize> = HashMap::new();
    let mut by_pid: HashMap<u32, usize> = HashMap::new();
    for (i, group) in groups.iter().enumerate() {
        for origin in &group.origins {
            by_origin.entry(origin.as_bytes()).or_insert(i);
        }
        for pid in &group.pids {
            by_pid.entry(*pid).or_insert(i);
        }
    }
    // What each process says of itself: Some(Some(group)), Some(None) (a
    // marker of someone else's), None (nothing: ask its parent).
    let mut own: HashMap<u32, Option<Option<usize>>> = HashMap::new();
    let mut found = HashMap::new();
    for &start in table.keys() {
        let (mut pid, mut steps) = (start, 0);
        let group = loop {
            let said = *own.entry(pid).or_insert_with(|| {
                if let Some(group) = by_pid.get(&pid) {
                    return Some(Some(*group));
                }
                environment(pid).and_then(|env| marker(&env).map(|origin| by_origin.get(origin).copied()))
            });
            if let Some(group) = said {
                break group;
            }
            match table.get(&pid) {
                Some(row) if row.parent > 1 && row.parent != pid && steps < 64 => pid = row.parent,
                _ => break None,
            }
            steps += 1;
        };
        if let Some(group) = group {
            found.insert(start, group);
        }
    }
    found
}

fn table() -> HashMap<u32, Row> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return HashMap::new();
    };
    dir.flatten()
        .filter_map(|entry| {
            let pid: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            Some((pid, parse_stat(&stat)?))
        })
        .collect()
}

/// A `Name:  123 kB` line's number, in bytes.
fn kilobytes(text: &str, name: &str) -> Option<u64> {
    let line = text.lines().find(|line| line.starts_with(name))?;
    Some(line[name.len()..].split_ascii_whitespace().next()?.parse::<u64>().ok()? * 1024)
}

fn memory(pid: u32) -> u64 {
    let read = |file: &str| std::fs::read_to_string(format!("/proc/{pid}/{file}")).ok();
    read("smaps_rollup")
        .and_then(|text| kilobytes(&text, "Pss:"))
        .or_else(|| read("status").and_then(|text| kilobytes(&text, "VmRSS:")))
        .unwrap_or(0)
}

/// A `name value` line's number (cgroup files, /proc/<pid>/io).
fn number(text: &str, name: &str) -> u64 {
    text.lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix(|c: char| c == ' ' || c == ':'))
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0)
}

/// `(read, written)` bytes at the storage layer so far.
fn disk(pid: u32) -> (u64, u64) {
    let text = std::fs::read_to_string(format!("/proc/{pid}/io")).unwrap_or_default();
    (number(&text, "read_bytes"), number(&text, "write_bytes"))
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Container {
    name: String,
    cgroup: String,
}

/// The container a conmon keeps (`-n` names it). An exec session's conmon
/// (`--exec`) keeps none: what it runs is a guest in another's container.
fn container(pid: u32, table: &HashMap<u32, Row>) -> Option<Container> {
    let args = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let args: Vec<&[u8]> = args.split(|b| *b == 0).collect();
    if args.iter().any(|a| *a == b"--exec" || *a == b"-e") {
        return None;
    }
    let name = args.windows(2).find(|w| w[0] == b"-n").map(|w| String::from_utf8_lossy(w[1]).into_owned())?;
    let child = table.iter().find(|(_, row)| row.parent == pid).map(|(child, _)| *child)?;
    let cgroup = std::fs::read_to_string(format!("/proc/{child}/cgroup")).ok()?;
    let cgroup = cgroup.lines().find_map(|line| line.strip_prefix("0::"))?.trim().to_owned();
    cgroup.contains("libpod-").then_some(Container { name, cgroup })
}

/// A cgroup's totals so far; memory as `podman stats` counts it (less the
/// page cache it can drop).
#[derive(Debug, Default, Clone, Copy)]
struct Counters {
    cpu_us: u64,
    memory: u64,
    read: u64,
    written: u64,
    processes: u32,
}

fn counters(cgroup: &str) -> Option<Counters> {
    let dir = PathBuf::from("/sys/fs/cgroup").join(cgroup.trim_start_matches('/'));
    let read = |file: &str| std::fs::read_to_string(dir.join(file)).ok();
    let current: u64 = read("memory.current")?.trim().parse().ok()?;
    let inactive = number(&read("memory.stat").unwrap_or_default(), "inactive_file");
    let (mut bytes_read, mut written) = (0, 0);
    for line in read("io.stat").unwrap_or_default().lines() {
        for field in line.split_ascii_whitespace() {
            if let Some(n) = field.strip_prefix("rbytes=") {
                bytes_read += n.parse::<u64>().unwrap_or(0);
            } else if let Some(n) = field.strip_prefix("wbytes=") {
                written += n.parse::<u64>().unwrap_or(0);
            }
        }
    }
    Some(Counters {
        cpu_us: number(&read("cpu.stat")?, "usage_usec"),
        memory: current.saturating_sub(inactive),
        read: bytes_read,
        written,
        processes: read("cgroup.procs").map_or(0, |p| p.lines().count() as u32),
    })
}

/// One reading: each process by `(pid, start)` with its group and disk
/// bytes, and each container a group keeps.
struct Sample {
    processes: HashMap<(u32, u64), (Row, usize, (u64, u64))>,
    containers: HashMap<Container, (usize, Counters)>,
}

fn sample(groups: &[Group]) -> Sample {
    let table = table();
    // ponytail: every process's environment is read on every sample (6 ms
    // for 700 processes here). Remember markers by (pid, start), as
    // crates/preview's Origins does, if a card ever samples faster.
    let owned = attribute(&table, groups, |pid| std::fs::read(format!("/proc/{pid}/environ")).ok());
    let mut containers = HashMap::new();
    for (&pid, &group) in &owned {
        if table[&pid].name == "conmon" {
            if let Some(c) = container(pid, &table) {
                if let Some(counters) = counters(&c.cgroup) {
                    containers.insert(c, (group, counters));
                }
            }
        }
    }
    // (A container's processes count in its cgroup's numbers, not apart.)
    let inside = |pid: u32| {
        !containers.is_empty()
            && std::fs::read_to_string(format!("/proc/{pid}/cgroup"))
                .is_ok_and(|cg| containers.keys().any(|c| cg.lines().any(|l| l.strip_prefix("0::").is_some_and(|p| p.trim().starts_with(&c.cgroup)))))
    };
    let processes = owned
        .into_iter()
        .filter(|(pid, _)| !inside(*pid))
        .map(|(pid, group)| {
            let row = table[&pid].clone();
            ((pid, row.start), (row, group, disk(pid)))
        })
        .collect();
    Sample { processes, containers }
}

/// What `groups` use, their CPU and disk measured over `window`.
pub fn measure(groups: &[Group], window: Duration) -> Usage {
    let before = sample(groups);
    let at = Instant::now();
    std::thread::sleep(window);
    let after = sample(groups);
    let secs = at.elapsed().as_secs_f64().max(0.001);
    // SAFETY: sysconf only reads a constant of the system's.
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let mut usage = Usage {
        cores: std::thread::available_parallelism().map_or(1, |n| n.get() as u32),
        ..Usage::default()
    };
    let mut amounts = vec![Amount::default(); groups.len()];
    let mut names: HashMap<String, Named> = HashMap::new();
    let mut count = |name: &str, amount: &Amount| {
        let named = names.entry(name.to_owned()).or_default();
        named.count += 1;
        named.amount.add(amount);
    };
    for (key, (row, group, (read, written))) in &after.processes {
        // (One started inside the window did all it has done inside it.)
        let (ticks, read0, written0) = before.processes.get(key).map_or((0, 0, 0), |(was, _, disk)| (was.ticks, disk.0, disk.1));
        let amount = Amount {
            cpu: row.ticks.saturating_sub(ticks) as f64 / hz / secs * 100.0,
            memory: memory(key.0),
            read: read.saturating_sub(read0) as f64 / secs,
            written: written.saturating_sub(written0) as f64 / secs,
            processes: 1,
        };
        amounts[*group].add(&amount);
        if !groups[*group].shared {
            usage.threads += row.threads;
            count(&row.name, &amount);
        }
    }
    for (c, (group, now)) in &after.containers {
        let was = before.containers.get(c).map_or(Counters::default(), |(_, was)| *was);
        let amount = Amount {
            cpu: now.cpu_us.saturating_sub(was.cpu_us) as f64 / 1e6 / secs * 100.0,
            memory: now.memory,
            read: now.read.saturating_sub(was.read) as f64 / secs,
            written: now.written.saturating_sub(was.written) as f64 / secs,
            processes: now.processes,
        };
        amounts[*group].add(&amount);
        if !groups[*group].shared {
            count(&c.name, &amount);
        }
    }
    for (i, (group, amount)) in groups.iter().zip(amounts).enumerate() {
        if amount.processes > 0 || i == 0 {
            if !group.shared {
                usage.total.add(&amount);
            }
            usage.groups.push((group.clone(), amount));
        }
    }
    usage.top = names
        .into_iter()
        .map(|(name, named)| Named { name, ..named })
        .collect();
    usage.top.sort_by(|a, b| b.amount.memory.cmp(&a.amount.memory).then_with(|| a.name.cmp(&b.name)));
    usage.top.truncate(TOP);
    usage
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(parent: u32) -> Row {
        Row { parent, start: 1, name: "x".into(), ticks: 0, threads: 1 }
    }

    fn group(origins: &[&str], pids: &[u32]) -> Group {
        Group { origins: origins.iter().map(|o| o.to_string()).collect(), pids: pids.to_vec(), ..Group::default() }
    }

    #[test]
    fn a_stat_line_is_read_past_a_name_with_brackets_and_spaces() {
        let stat = "42 (a) (b c) S 7 42 42 0 -1 4194560 100 0 0 0 30 12 0 0 20 0 5 0 9001 1000 200 18446744073709551615";
        assert_eq!(parse_stat(stat), Some(Row { parent: 7, start: 9001, name: "a) (b c".into(), ticks: 42, threads: 5 }));
        assert_eq!(parse_stat("42 (gone) Z 7 42 42 0 -1 0 0 0 0 0 1 1 0 0 20 0 1 0 5 0 0"), None);
    }

    #[test]
    fn a_process_is_its_nearest_marked_or_rooted_ancestor_s() {
        // 10 (chat a) -> 11 (no marker) -> 12 (unreadable); 20 (chat b) -> 21
        // (chat a again: a chat's own engine starting another chat's agent);
        // 30 (a service's terminal) -> 31 (its conmon: same marker) -> 32 (the
        // container: unreadable); 40 (the VM: no marker, a root pid) -> 41;
        // 50: the user's own, 51 under it.
        let table: HashMap<u32, Row> =
            [(10, 1), (11, 10), (12, 11), (20, 1), (21, 20), (30, 1), (31, 30), (32, 31), (40, 1), (41, 40), (50, 1), (51, 50)].map(|(pid, parent)| (pid, row(parent))).into();
        let environment = |pid: u32| match pid {
            10 | 21 => Some(b"PATH=/bin\0ZERON_ORIGIN=chat:a\0".to_vec()),
            20 => Some(b"XZERON_ORIGIN=chat:a\0ZERON_ORIGIN=chat:b\0".to_vec()),
            30 | 31 => Some(b"ZERON_ORIGIN=terminal:t\0".to_vec()),
            11 | 40 | 41 | 50 | 51 => Some(b"PATH=/bin\0ZERON_ORIGIN=\0".to_vec()),
            _ => None,
        };
        let groups = [group(&["chat:a"], &[]), group(&["terminal:t"], &[]), group(&[], &[40])];
        let of = |g: usize| {
            let mut pids: Vec<u32> = attribute(&table, &groups, environment).into_iter().filter(|(_, i)| *i == g).map(|(p, _)| p).collect();
            pids.sort();
            pids
        };
        assert_eq!(of(0), [10, 11, 12, 21]);
        assert_eq!(of(1), [30, 31, 32]);
        assert_eq!(of(2), [40, 41]);
        // chat b's own (20) is nobody's here: its marker wasn't asked for.
        assert!(!attribute(&table, &groups, environment).contains_key(&20));
    }

    #[test]
    fn cgroup_and_io_lines_are_read_by_name() {
        assert_eq!(number("anon 5\ninactive_file 7\nfile 9", "inactive_file"), 7);
        assert_eq!(number("rchar: 1\nread_bytes: 4096\nwrite_bytes: 0", "read_bytes"), 4096);
        assert_eq!(number("usage_usec 123\nuser_usec 100", "usage_usec"), 123);
        assert_eq!(number("usage_usec_x 5", "usage_usec"), 0);
    }

    #[test]
    fn this_process_is_measured_when_marked() {
        // (Marked only when the test is run so: `ZERON_ORIGIN=chat:t cargo test`.)
        if std::env::var("ZERON_ORIGIN").as_deref() != Ok("chat:t") {
            return;
        }
        // Its parent (cargo, marked too) as a shared group's root: measured,
        // but not in the chat's own totals.
        let parent = parse_stat(&std::fs::read_to_string("/proc/self/stat").unwrap()).unwrap().parent;
        let shared = Group { shared: true, ..group(&[], &[parent]) };
        let usage = measure(&[group(&["chat:t"], &[]), shared], Duration::from_millis(50));
        assert!(usage.total.processes >= 1 && usage.total.memory > 0 && usage.threads >= 1, "{usage:?}");
        assert_eq!(usage.groups.len(), 2, "{usage:?}");
        assert_eq!(usage.total, usage.groups[0].1, "the shared group isn't in the totals");
        assert!(usage.groups[1].1.processes >= 1);
        assert!(usage.top.iter().all(|t| t.count <= usage.total.processes));
    }
}
