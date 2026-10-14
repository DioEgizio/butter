//! Programs running in bottles. Each one runs under a keeper (see
//! [`crate::supervisor`]), so Butter knows every process it starts, shows its
//! terminal, and finds it again after a restart.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::rc::{Rc, Weak};
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::bottle::{self, Backend, Bottle};
use crate::procs::{self, Id};
use crate::supervisor::{self, Report};
use crate::util;

/// Output kept per program for terminals opened later.
const KEEP_OUTPUT: usize = 4 << 20;

/// What a bottle's `run/` directory holds for each keeper.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    keeper: Id,
    name: String,
    log: PathBuf,
    #[serde(default)]
    program: Option<Id>,
}

/// How a program is doing.
#[derive(Debug, Clone)]
pub enum Status {
    Starting,
    Running,
    Exited(ExitStatus),
    Failed(String),
    /// It ended while another Butter was watching it; how is unknown.
    Ended,
}

impl Status {
    pub fn is_running(&self) -> bool {
        matches!(self, Status::Starting | Status::Running)
    }

    pub fn describe(&self) -> String {
        match self {
            Status::Starting => "Starting".into(),
            Status::Running => "Running".into(),
            Status::Exited(status) => match (status.code(), status.signal()) {
                (Some(0), _) => "Exited".into(),
                (Some(code), _) => format!("Exited with code {code}"),
                (_, Some(signal)) => format!("Ended by {}", procs::signal_name(signal)),
                _ => "Exited".into(),
            },
            Status::Failed(message) => format!("Failed: {message}"),
            Status::Ended => "Exited".into(),
        }
    }
}

/// Butter's ends of a keeper it started: input for the terminal, and the
/// control socket.
struct Link {
    input: File,
    control: UnixStream,
}

/// One program started in a bottle, and everything it started.
pub struct Session {
    /// Unique while Butter runs.
    pub id: u64,
    pub bottle: String,
    pub name: String,
    pub keeper: Id,
    pub log: PathBuf,
    record: PathBuf,
    program: Cell<Option<Id>>,
    pts: RefCell<Option<PathBuf>>,
    status: RefCell<Status>,
    /// Whether the keeper runs, meaning some of the program's processes may.
    alive: Cell<bool>,
    /// None when the keeper was started by an earlier Butter: its output is
    /// then read from the log, and it takes no input.
    link: Option<Link>,
    output: RefCell<Vec<u8>>,
    log_read: Cell<u64>,
    watchers: RefCell<Vec<Watcher>>,
    on_end: RefCell<Option<OnEnd>>,
}

type Watcher = Box<dyn Fn(&[u8]) -> bool>;
type OnEnd = Box<dyn FnOnce(&Session)>;

impl Session {
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    pub fn is_alive(&self) -> bool {
        self.alive.get()
    }

    /// The program itself: `rosemary run`, below the keeper.
    pub fn program(&self) -> Option<Id> {
        self.program.get()
    }

    /// The terminal the program was given.
    pub fn pts(&self) -> Option<PathBuf> {
        self.pts.borrow().clone()
    }

    /// Whether this Butter started it, and so relays its terminal live.
    pub fn is_linked(&self) -> bool {
        self.link.is_some()
    }

    /// Whether typing reaches the program.
    pub fn takes_input(&self) -> bool {
        self.link.is_some() && self.alive.get() && self.status.borrow().is_running()
    }

    /// Output so far (or its most recent part).
    pub fn output(&self) -> Vec<u8> {
        self.output.borrow().clone()
    }

    /// Call `watcher` with new output until it returns false.
    pub fn watch(&self, watcher: impl Fn(&[u8]) -> bool + 'static) {
        self.watchers.borrow_mut().push(Box::new(watcher));
    }

    /// Type into the program's terminal. Dropped if the program is not reading.
    pub fn write(&self, data: &[u8]) {
        if let Some(link) = &self.link {
            let _ = (&link.input).write(data);
        }
    }

    /// Tell the program its terminal's size.
    pub fn resize(&self, rows: u16, columns: u16) {
        if let Some(link) = &self.link {
            let message = supervisor::resize_message(rows, columns);
            // SAFETY: plain syscall with a valid buffer; never blocks or raises SIGPIPE.
            unsafe {
                libc::send(
                    link.control.as_raw_fd(),
                    message.as_ptr().cast(),
                    message.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
        }
    }

    fn push_output(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        {
            let mut output = self.output.borrow_mut();
            output.extend_from_slice(data);
            if output.len() > KEEP_OUTPUT {
                // Cut at a line break, so the kept part starts cleanly.
                let excess = output.len() - KEEP_OUTPUT * 3 / 4;
                let cut = output[excess..]
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(excess, |i| excess + i + 1);
                output.drain(..cut);
            }
        }
        let watchers = std::mem::take(&mut *self.watchers.borrow_mut());
        let mut kept: Vec<_> = watchers.into_iter().filter(|w| w(data)).collect();
        let mut watchers = self.watchers.borrow_mut();
        kept.append(&mut watchers);
        *watchers = kept;
    }

    fn save_record(&self) {
        let record = Record {
            keeper: self.keeper,
            name: self.name.clone(),
            log: self.log.clone(),
            program: self.program.get(),
        };
        if let Ok(text) = toml::to_string(&record)
            && let Some(dir) = self.record.parent()
            && std::fs::create_dir_all(dir).is_ok()
        {
            let _ = util::write_atomic(&self.record, text.as_bytes());
        }
    }

    /// The keeper is gone: nothing of the program is left to track.
    fn end(&self) {
        self.alive.set(false);
        let _ = std::fs::remove_file(&self.record);
        if self.status.borrow().is_running() {
            self.finish(Status::Ended);
        }
    }

    fn finish(&self, status: Status) {
        *self.status.borrow_mut() = status;
        if let Some(on_end) = self.on_end.borrow_mut().take() {
            on_end(self);
        }
    }

    /// For a keeper an earlier Butter started: follow the log, and notice
    /// when things end.
    fn poll(&self) {
        if self.link.is_some() || !self.alive.get() {
            return;
        }
        if let Ok(mut file) = File::open(&self.log)
            && file
                .seek(std::io::SeekFrom::Start(self.log_read.get()))
                .is_ok()
        {
            let mut data = Vec::new();
            if let Ok(n) = file.read_to_end(&mut data) {
                self.log_read.set(self.log_read.get() + n as u64);
                self.push_output(&data);
            }
        }
        if self.status.borrow().is_running() && !self.program.get().is_some_and(procs::is_alive) {
            self.finish(Status::Ended);
        }
        if !procs::is_alive(self.keeper) {
            self.end();
        }
    }
}

/// A process in a bottle, as listed.
#[derive(Debug, Clone)]
pub struct Process {
    pub id: Id,
    pub name: String,
    pub command: String,
    pub state: char,
    /// Whether it reads or writes the program's terminal.
    pub on_terminal: bool,
}

/// One line of a bottle's process list: a program, or one of the processes
/// it started.
pub struct Entry {
    pub session: Rc<Session>,
    /// For the program's own entry, its process while it runs.
    pub process: Option<Process>,
    pub is_program: bool,
    /// How far to indent it: 0 for the program.
    pub level: usize,
}

#[derive(Clone, Default)]
pub struct Sessions(Rc<Inner>);

#[derive(Default)]
struct Inner {
    list: RefCell<Vec<Rc<Session>>>,
    next: Cell<u64>,
    listeners: RefCell<Vec<Box<dyn Fn() -> bool>>>,
}

impl Sessions {
    /// The registry, with the programs earlier Butters left running.
    pub fn new() -> Sessions {
        let this = Sessions::default();
        for bottle in bottle::list() {
            this.adopt(&bottle);
        }
        this
    }

    /// Call `listener` whenever programs start, end or are dismissed, until
    /// it returns false.
    pub fn connect_changed(&self, listener: impl Fn() -> bool + 'static) {
        self.0.listeners.borrow_mut().push(Box::new(listener));
    }

    fn changed(&self) {
        let listeners = std::mem::take(&mut *self.0.listeners.borrow_mut());
        let mut kept: Vec<_> = listeners.into_iter().filter(|l| l()).collect();
        let mut listeners = self.0.listeners.borrow_mut();
        kept.append(&mut listeners);
        *listeners = kept;
    }

    fn weak(&self) -> Weak<Inner> {
        Rc::downgrade(&self.0)
    }

    fn next_id(&self) -> u64 {
        let id = self.0.next.get() + 1;
        self.0.next.set(id);
        id
    }

    pub fn get(&self, id: u64) -> Option<Rc<Session>> {
        self.0.list.borrow().iter().find(|s| s.id == id).cloned()
    }

    /// A bottle's programs, oldest first.
    pub fn of(&self, bottle: &str) -> Vec<Rc<Session>> {
        self.0
            .list
            .borrow()
            .iter()
            .filter(|s| s.bottle == bottle)
            .cloned()
            .collect()
    }

    /// Start `target` in `bottle` under a keeper. `on_end` is called once,
    /// when the program exits or fails to start.
    pub fn launch(
        &self,
        bottle: &Bottle,
        target: &Path,
        args: &[String],
        backend: Backend,
        name: &str,
        on_end: impl FnOnce(&Session) + 'static,
    ) -> anyhow::Result<Rc<Session>> {
        let command = bottle.run_command(target, args, backend)?;
        let (log_path, mut log) = new_log(&bottle.logs())?;
        // A terminal wants carriage returns too.
        write!(log, "$ {}\r\n", crate::process::describe(&command))?;
        drop(log);
        // Not fatal: only the Last Run Log row reads it.
        let last = bottle.last_log();
        let _ = std::fs::remove_file(&last);
        if let Some(file_name) = log_path.file_name() {
            let _ = std::os::unix::fs::symlink(file_name, &last);
        }

        let (ours, theirs) = UnixStream::pair()?;
        let mut keeper = Command::new("/proc/self/exe");
        keeper
            .arg0("butter")
            .args(supervisor::arguments(&log_path, command.get_program()))
            .args(command.get_args())
            // VTE emulates xterm; the bottle's own variables may still override it.
            .env("TERM", "xterm-256color")
            .env("COLORTERM", "truecolor");
        for (key, value) in command.get_envs() {
            match value {
                Some(value) => keeper.env(key, value),
                None => keeper.env_remove(key),
            };
        }
        if let Some(dir) = command.get_current_dir() {
            keeper.current_dir(dir);
        }
        keeper.stdin(Stdio::piped()).stdout(Stdio::piped());
        let fd = theirs.as_raw_fd();
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            keeper.pre_exec(move || {
                if fd == supervisor::CONTROL_FD {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                } else if libc::dup2(fd, supervisor::CONTROL_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                // Out of Butter's session: closing the terminal Butter was
                // started from, or Butter quitting, leaves programs running.
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = keeper
            .spawn()
            .context("could not start the program's keeper")?;
        drop(theirs);
        let pid = child.id() as i32;
        let keeper_id = procs::stat(pid).map_or(Id { pid, start: 0 }, |s| s.id());
        let input = File::from(std::os::fd::OwnedFd::from(
            child.stdin.take().context("no keeper input")?,
        ));
        let output = child.stdout.take().context("no keeper output")?;
        supervisor::set_nonblocking(input.as_raw_fd());

        let session = Rc::new(Session {
            id: self.next_id(),
            bottle: bottle.id.clone(),
            name: name.to_string(),
            keeper: keeper_id,
            log: log_path,
            record: bottle.records().join(format!("{pid}.toml")),
            program: Cell::new(None),
            pts: RefCell::new(None),
            status: RefCell::new(Status::Starting),
            alive: Cell::new(true),
            link: Some(Link {
                input,
                control: ours.try_clone()?,
            }),
            output: RefCell::new(Vec::new()),
            log_read: Cell::new(0),
            watchers: RefCell::default(),
            on_end: RefCell::new(Some(Box::new(on_end))),
        });
        session.save_record();
        self.0.list.borrow_mut().push(session.clone());

        enum Message {
            Output(Vec<u8>),
            Report(Report),
            Ended,
        }
        let (sender, receiver) = async_channel::unbounded();
        let output_thread = {
            let sender = sender.clone();
            std::thread::spawn(move || {
                let mut output = output;
                let mut buf = vec![0u8; 64 * 1024];
                while let Ok(n @ 1..) = output.read(&mut buf) {
                    if sender
                        .send_blocking(Message::Output(buf[..n].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
            })
        };
        std::thread::spawn(move || {
            for line in BufReader::new(ours).lines() {
                let Ok(line) = line else { break };
                if let Some(report) = Report::parse(&line) {
                    let _ = sender.send_blocking(Message::Report(report));
                }
            }
            // The keeper closes its socket only by exiting.
            let _ = child.wait();
            let _ = output_thread.join();
            let _ = sender.send_blocking(Message::Ended);
        });

        let weak = Rc::downgrade(&session);
        let sessions = self.weak();
        gtk::glib::spawn_future_local(async move {
            while let Ok(message) = receiver.recv().await {
                let Some(session) = weak.upgrade() else { break };
                let sessions = sessions.upgrade().map(Sessions);
                match message {
                    Message::Output(data) => {
                        session.push_output(&data);
                        continue;
                    }
                    Message::Report(Report::Started { pid, pts }) => {
                        session.program.set(procs::stat(pid).map(|s| s.id()));
                        *session.pts.borrow_mut() = Some(pts.into());
                        if session.status.borrow().is_running() {
                            *session.status.borrow_mut() = Status::Running;
                        }
                        session.save_record();
                    }
                    Message::Report(Report::Failed(message)) => {
                        session.finish(Status::Failed(message));
                    }
                    Message::Report(Report::Exited(status)) => {
                        session.finish(Status::Exited(ExitStatus::from_raw(status)));
                    }
                    Message::Ended => session.end(),
                }
                if let Some(sessions) = sessions {
                    sessions.changed();
                }
            }
        });
        self.changed();
        Ok(session)
    }

    /// Take over the keepers an earlier Butter started in `bottle`, and
    /// forget the ones that have ended.
    fn adopt(&self, bottle: &Bottle) {
        let mut logs = HashSet::new();
        if let Ok(entries) = std::fs::read_dir(bottle.records()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let record: Option<Record> = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| toml::from_str(&text).ok());
                let Some(record) = record.filter(|r| procs::is_alive(r.keeper)) else {
                    let _ = std::fs::remove_file(&path);
                    continue;
                };
                let running = record.program.is_some_and(procs::is_alive);
                // The terminal the program was given is still its stdin.
                let pts = record
                    .program
                    .and_then(|p| std::fs::read_link(format!("/proc/{}/fd/0", p.pid)).ok())
                    .filter(|p| p.starts_with("/dev/pts"));
                logs.insert(record.log.clone());
                let session = Rc::new(Session {
                    id: self.next_id(),
                    bottle: bottle.id.clone(),
                    name: record.name,
                    keeper: record.keeper,
                    log: record.log,
                    record: path,
                    program: Cell::new(record.program),
                    pts: RefCell::new(pts),
                    status: RefCell::new(if running {
                        Status::Running
                    } else {
                        Status::Ended
                    }),
                    alive: Cell::new(true),
                    link: None,
                    output: RefCell::new(Vec::new()),
                    log_read: Cell::new(0),
                    watchers: RefCell::default(),
                    on_end: RefCell::new(None),
                });
                session.poll();
                self.0.list.borrow_mut().push(session);
            }
        }
        // Logs of programs that ended while Butter was not running, except
        // the one Last Run Log shows.
        let last = last_log_target(bottle);
        if let Ok(entries) = std::fs::read_dir(bottle.logs()) {
            for entry in entries.flatten() {
                let path = entry.path();
                let is_run_log = entry.file_name().to_string_lossy().starts_with("run-");
                if is_run_log && !logs.contains(&path) && last.as_deref() != Some(&path) {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    /// Catch up with keepers an earlier Butter started. Called regularly
    /// while their processes are shown.
    pub fn poll(&self) {
        let list = self.0.list.borrow().clone();
        let before: Vec<_> = list
            .iter()
            .map(|s| (s.is_alive(), s.status().is_running()))
            .collect();
        for session in &list {
            session.poll();
        }
        let after: Vec<_> = list
            .iter()
            .map(|s| (s.is_alive(), s.status().is_running()))
            .collect();
        if before != after {
            self.changed();
        }
    }

    /// Forget a program that has ended, and its log.
    pub fn dismiss(&self, id: u64) {
        let Some(session) = self.get(id) else { return };
        if session.is_alive() {
            return;
        }
        self.0.list.borrow_mut().retain(|s| s.id != id);
        let last = bottle::load(&session.bottle).and_then(|b| last_log_target(&b));
        if last.as_ref() != Some(&session.log) {
            let _ = std::fs::remove_file(&session.log);
        }
        self.changed();
    }

    /// Forget every program in `bottle` that has ended.
    pub fn dismiss_ended(&self, bottle: &str) {
        for session in self.of(bottle) {
            if !session.is_alive() {
                self.dismiss(session.id);
            }
        }
    }

    /// The processes in `bottle`, program by program.
    pub fn snapshot(&self, bottle: &str) -> Vec<Entry> {
        let table = procs::table();
        let mut entries = Vec::new();
        for session in self.of(bottle) {
            let pts = session.pts();
            let below = if session.is_alive() {
                procs::descendants(&table, session.keeper.pid)
            } else {
                Vec::new()
            };
            let describe = |stat: &procs::Stat| {
                let argv = procs::cmdline(stat.pid);
                Process {
                    id: stat.id(),
                    name: display_name(&argv, &stat.comm),
                    command: if argv.is_empty() {
                        format!("[{}]", stat.comm)
                    } else {
                        argv.join(" ")
                    },
                    state: stat.state,
                    on_terminal: pts
                        .as_deref()
                        .is_some_and(|pts| procs::uses_terminal(stat.pid, pts)),
                }
            };
            let program = session.program();
            entries.push(Entry {
                session: session.clone(),
                process: below
                    .iter()
                    .find(|(s, _)| Some(s.id()) == program)
                    .map(|(s, _)| describe(s)),
                is_program: true,
                level: 0,
            });
            // Below the program, levels count from it; processes that left
            // it (to the keeper) count from the keeper.
            let mut program_depth = None;
            for (stat, depth) in &below {
                if program_depth.is_some_and(|d| *depth <= d) {
                    program_depth = None;
                }
                if Some(stat.id()) == program {
                    program_depth = Some(*depth);
                    continue;
                }
                entries.push(Entry {
                    session: session.clone(),
                    process: Some(describe(stat)),
                    is_program: false,
                    level: depth - program_depth.unwrap_or(0),
                });
            }
        }
        entries
    }

    /// Whether a program this Butter started, in any bottle, still runs.
    pub fn any_linked_running(&self) -> bool {
        self.0
            .list
            .borrow()
            .iter()
            .any(|s| s.is_linked() && s.status().is_running())
    }

    /// Whether anything Butter started in `bottle` still runs.
    pub fn is_running(&self, bottle: &str) -> bool {
        self.of(bottle).iter().any(|s| s.is_alive())
    }

    /// Every process Butter started in `bottle`, and all they started.
    fn tracked(&self, bottle: &str) -> Vec<Id> {
        self.of(bottle)
            .iter()
            .filter(|s| s.is_alive())
            .map(|s| s.keeper)
            .collect()
    }

    /// Kill everything running in `bottle` at once, with SIGKILL. Returns
    /// how many processes were killed.
    pub async fn force_stop(&self, bottle: &str) -> usize {
        let keepers = self.tracked(bottle);
        let killed = crate::process::background(move || {
            let mut killed = HashSet::new();
            // Again until nothing is left: a process may fork while we kill.
            for _ in 0..20 {
                let table = procs::table();
                let targets: Vec<Id> = keepers
                    .iter()
                    .filter(|k| table.get(&k.pid).is_some_and(|s| s.start == k.start))
                    .flat_map(|k| procs::descendants(&table, k.pid))
                    .map(|(s, _)| s.id())
                    .collect();
                if targets.is_empty() {
                    break;
                }
                for id in targets {
                    if procs::send(id, libc::SIGKILL).is_ok() {
                        killed.insert(id);
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            killed.len()
        })
        .await;
        self.poll();
        killed
    }
}

/// The log `last-run.log` links to.
fn last_log_target(bottle: &Bottle) -> Option<PathBuf> {
    let target = std::fs::read_link(bottle.last_log()).ok()?;
    Some(bottle.logs().join(target.file_name()?))
}

/// A new log file in `dir`, named for now.
fn new_log(dir: &Path) -> anyhow::Result<(PathBuf, File)> {
    std::fs::create_dir_all(dir)?;
    let now = util::now();
    for n in 0.. {
        let path = dir.join(format!("run-{now}-{n}.log"));
        match File::options().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    unreachable!()
}

/// A short name for a process, from its arguments: Rosemary's own processes
/// are named for what they run.
pub fn display_name(argv: &[String], comm: &str) -> String {
    let base = |a: &str| {
        Path::new(a)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| a.to_string())
    };
    let Some(first) = argv.first() else {
        return comm.to_string();
    };
    // The launcher of an official build may start it through a loader.
    let Some(at) = argv.iter().position(|a| base(a) == "rosemary") else {
        return base(first);
    };
    let mut rest = argv[at + 1..].iter().map(String::as_str);
    match rest.next() {
        Some("run") => rest
            .find(|a| !a.starts_with('-'))
            .map(|target| bottle::program_name(Path::new(target)))
            .unwrap_or_else(|| "Rosemary".into()),
        Some(sub) if !sub.starts_with('-') => format!("Rosemary {sub}"),
        _ => "Rosemary".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn names_processes() {
        assert_eq!(display_name(&[], "kworker"), "kworker");
        assert_eq!(display_name(&argv(&["/usr/bin/top", "-b"]), "top"), "top");
        assert_eq!(
            display_name(
                &argv(&["/r/bin/rosemary", "run", "--native", "/x/Foo.app"]),
                "rosemary"
            ),
            "Foo"
        );
        assert_eq!(
            display_name(
                &argv(&["/lib/ld-linux.so", "/r/Rosemary/bin/rosemary", "server"]),
                "ld"
            ),
            "Rosemary server"
        );
        assert_eq!(
            display_name(&argv(&["rosemary", "--version"]), "rosemary"),
            "Rosemary"
        );
    }

    #[test]
    fn statuses() {
        assert_eq!(
            Status::Exited(ExitStatus::from_raw(1 << 8)).describe(),
            "Exited with code 1"
        );
        assert_eq!(
            Status::Exited(ExitStatus::from_raw(libc::SIGKILL)).describe(),
            "Ended by SIGKILL"
        );
        assert!(Status::Starting.is_running());
        assert!(!Status::Ended.is_running());
    }
}
