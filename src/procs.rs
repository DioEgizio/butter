//! What `/proc` says about processes, and signalling them without hitting a
//! stranger that reused a pid.

use std::collections::HashMap;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd};
use std::path::Path;

/// A process, told apart from any later one with the same pid by when it
/// started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Id {
    pub pid: i32,
    /// Field 22 of `/proc/PID/stat`: clock ticks after boot.
    pub start: u64,
}

/// The fields of `/proc/PID/stat` Butter uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub pid: i32,
    pub comm: String,
    pub state: char,
    pub ppid: i32,
    pub start: u64,
}

impl Stat {
    pub fn id(&self) -> Id {
        Id {
            pid: self.pid,
            start: self.start,
        }
    }
}

/// Parse a `/proc/PID/stat` line. The command name is in parentheses and may
/// itself contain spaces and parentheses, so fields are counted from the last `)`.
pub fn parse_stat(text: &str) -> Option<Stat> {
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    let pid = text[..open].trim().parse().ok()?;
    let comm = text.get(open + 1..close)?.to_string();
    // Fields from the third (state) on.
    let rest: Vec<&str> = text.get(close + 1..)?.split_whitespace().collect();
    Some(Stat {
        pid,
        comm,
        state: rest.first()?.chars().next()?,
        ppid: rest.get(1)?.parse().ok()?,
        start: rest.get(19)?.parse().ok()?,
    })
}

pub fn stat(pid: i32) -> Option<Stat> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Whether `id` is still the process it was.
pub fn is_alive(id: Id) -> bool {
    stat(id.pid).is_some_and(|s| s.start == id.start && s.state != 'Z')
}

/// Every process this user can see, by pid.
pub fn table() -> HashMap<i32, Stat> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter_map(stat)
        .map(|s| (s.pid, s))
        .collect()
}

/// The living descendants of `root` with their depth below it (children
/// are 1), each followed by its own descendants.
pub fn descendants(table: &HashMap<i32, Stat>, root: i32) -> Vec<(Stat, usize)> {
    let mut children: HashMap<i32, Vec<&Stat>> = HashMap::new();
    for stat in table.values() {
        children.entry(stat.ppid).or_default().push(stat);
    }
    for list in children.values_mut() {
        // Newest first, so the stack below pops the oldest first.
        list.sort_by_key(|s| std::cmp::Reverse((s.start, s.pid)));
    }
    let mut found = Vec::new();
    let mut stack: Vec<(&Stat, usize)> = children
        .get(&root)
        .into_iter()
        .flatten()
        .map(|s| (*s, 1))
        .collect();
    while let Some((stat, depth)) = stack.pop() {
        if stat.state != 'Z' {
            found.push((stat.clone(), depth));
        }
        for child in children.get(&stat.pid).into_iter().flatten() {
            stack.push((child, depth + 1));
        }
    }
    found
}

/// A process's arguments, empty for kernel threads and the dead.
pub fn cmdline(pid: i32) -> Vec<String> {
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .map(|bytes| {
            bytes
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// Whether the process reads or writes the terminal at `pts`.
pub fn uses_terminal(pid: i32, pts: &Path) -> bool {
    (0..3).any(|fd| {
        std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).is_ok_and(|target| target == pts)
    })
}

pub fn state_label(state: char) -> &'static str {
    match state {
        'R' => "Running",
        'S' | 'I' => "Sleeping",
        'D' => "Waiting",
        'T' => "Stopped",
        't' => "Being Traced",
        'Z' | 'X' => "Exited",
        _ => "Unknown",
    }
}

/// Send `signal` to `id`, unless its pid now belongs to someone else.
///
/// A pidfd pins the process first, so checking its start time afterwards
/// proves the signal reaches the process that was listed.
pub fn send(id: Id, signal: i32) -> io::Result<()> {
    // SAFETY: plain syscalls; the descriptor is owned right away.
    let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, id.pid, 0) };
    if raw < 0 {
        let error = io::Error::last_os_error();
        if !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EPERM)) {
            return Err(error);
        }
        // Kernels before 5.3, or a sandbox that forbids pidfds: check, then
        // signal, and accept the small race.
        if !is_alive(id) {
            return Err(io::Error::from_raw_os_error(libc::ESRCH));
        }
        // SAFETY: plain syscall.
        return match unsafe { libc::kill(id.pid, signal) } {
            0 => Ok(()),
            _ => Err(io::Error::last_os_error()),
        };
    }
    // SAFETY: pidfd_open returned a new descriptor we now own.
    let pidfd = unsafe { OwnedFd::from_raw_fd(raw as i32) };
    if stat(id.pid).is_none_or(|s| s.start != id.start) {
        return Err(io::Error::from_raw_os_error(libc::ESRCH));
    }
    use std::os::fd::AsRawFd;
    // SAFETY: plain syscall on a descriptor we own.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            pidfd.as_raw_fd(),
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0,
        )
    };
    match result {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

/// Signals offered in the interface, in sections: number, name, description.
pub const SIGNALS: &[&[(i32, &str, &str)]] = &[
    &[
        (libc::SIGTERM, "SIGTERM", "Terminate"),
        (libc::SIGINT, "SIGINT", "Interrupt"),
        (libc::SIGHUP, "SIGHUP", "Hang Up"),
        (libc::SIGQUIT, "SIGQUIT", "Quit"),
    ],
    &[
        (libc::SIGSTOP, "SIGSTOP", "Pause"),
        (libc::SIGCONT, "SIGCONT", "Continue"),
    ],
    &[
        (libc::SIGUSR1, "SIGUSR1", "User Signal 1"),
        (libc::SIGUSR2, "SIGUSR2", "User Signal 2"),
    ],
    &[(libc::SIGKILL, "SIGKILL", "Kill")],
];

pub fn signal_name(signal: i32) -> String {
    SIGNALS
        .iter()
        .flat_map(|section| section.iter())
        .find(|(n, _, _)| *n == signal)
        .map(|(_, name, _)| name.to_string())
        .unwrap_or_else(|| format!("signal {signal}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_awkward_names() {
        let line = "4242 (my (odd) prog) S 17 4242 4242 0 -1 4194560 100 0 0 0 1 2 0 0 20 0 3 0 98765 1000 50 18446744073709551615";
        let stat = parse_stat(line).unwrap();
        assert_eq!(stat.pid, 4242);
        assert_eq!(stat.comm, "my (odd) prog");
        assert_eq!(stat.state, 'S');
        assert_eq!(stat.ppid, 17);
        assert_eq!(stat.start, 98765);
        assert!(parse_stat("garbage").is_none());
    }

    #[test]
    fn reads_this_process() {
        let me = stat(std::process::id() as i32).unwrap();
        assert!(is_alive(me.id()));
        assert!(!is_alive(Id {
            start: me.start + 1,
            ..me.id()
        }));
        assert!(!cmdline(me.pid).is_empty());
    }

    #[test]
    fn finds_descendants_and_signals_safely() {
        let mut child = std::process::Command::new("sh")
            .args(["-c", "sh -c 'sleep 30; true' & wait"])
            .spawn()
            .unwrap();
        let root = child.id() as i32;
        let mut found = Vec::new();
        for _ in 0..100 {
            found = descendants(&table(), root);
            if found.len() >= 2 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!((found[0].0.ppid, found[0].1), (root, 1), "parents first");
        let (sleeper, depth) = &found[1];
        assert_eq!((sleeper.ppid, *depth), (found[0].0.pid, 2));

        // A stale start time must not reach the process.
        let stale = Id {
            start: sleeper.start + 1,
            ..sleeper.id()
        };
        assert!(send(stale, libc::SIGKILL).is_err());
        assert!(is_alive(sleeper.id()));

        send(sleeper.id(), libc::SIGKILL).unwrap();
        assert!(
            child.wait().unwrap().success(),
            "the outer shell's wait returns"
        );
    }
}
