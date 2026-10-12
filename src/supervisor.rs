//! `butter --supervise`: the small keeper every program in a bottle runs under.
//!
//! Butter starts programs through a copy of itself in this mode. The keeper:
//!
//! - is a child subreaper, so everything the program starts stays below it,
//!   even daemons that detach, such as Rosemary's process server. Butter finds
//!   a bottle's processes by walking down from its keepers, never by name;
//! - gives the program a pseudo-terminal of its own. The program sees an
//!   ordinary terminal, while the keeper relays what it writes to Butter (on
//!   the keeper's stdout) and to a log file, and Butter's input (the keeper's
//!   stdin) back to it;
//! - talks with Butter on [`CONTROL_FD`], one line per message: it says when
//!   the program started and how it ended, and is told when the terminal
//!   is resized.
//!
//! The keeper outlives Butter. If Butter quits, programs keep running and
//! their output still reaches the log; the next Butter finds the keeper again
//! from the record it left in the bottle.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// The first argument that selects this mode.
pub const FLAG: &str = "--supervise";

/// The socket the keeper and Butter exchange messages on.
pub const CONTROL_FD: RawFd = 3;

/// A message from the keeper to Butter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// The program started with this pid, on this terminal.
    Started { pid: i32, pts: String },
    /// The program could not be started.
    Failed(String),
    /// The program ended with this wait status.
    Exited(i32),
}

impl Report {
    pub fn parse(line: &str) -> Option<Report> {
        let (kind, rest) = line.split_once(' ')?;
        match kind {
            "started" => {
                let (pid, pts) = rest.split_once(' ')?;
                Some(Report::Started {
                    pid: pid.parse().ok()?,
                    pts: pts.to_string(),
                })
            }
            "failed" => Some(Report::Failed(rest.to_string())),
            "exited" => rest.parse().ok().map(Report::Exited),
            _ => None,
        }
    }

    fn line(&self) -> String {
        match self {
            Report::Started { pid, pts } => format!("started {pid} {pts}\n"),
            // Messages are single lines.
            Report::Failed(message) => format!("failed {}\n", message.replace('\n', " ")),
            Report::Exited(status) => format!("exited {status}\n"),
        }
    }
}

/// `resize ROWS COLUMNS`, from Butter.
pub fn resize_message(rows: u16, columns: u16) -> String {
    format!("resize {rows} {columns}\n")
}

/// The arguments that start a keeper for `program`.
pub fn arguments(log: &std::path::Path, program: &std::ffi::OsStr) -> Vec<OsString> {
    vec![
        FLAG.into(),
        "--log".into(),
        log.into(),
        "--".into(),
        program.into(),
    ]
}

/// Run as the keeper; `args` are the ones after [`FLAG`]. Returns the exit code.
pub fn main(args: Vec<OsString>) -> i32 {
    let mut args = args.into_iter();
    let mut log = None;
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--log") => log = args.next(),
            Some("--") => break,
            _ => {
                eprintln!("butter --supervise: unexpected {arg:?}");
                return 2;
            }
        }
    }
    let Some(program) = args.next() else {
        eprintln!("butter --supervise: no program");
        return 2;
    };
    let rest: Vec<OsString> = args.collect();

    let is_open = |fd| unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0;
    // SAFETY: the descriptors were handed to us by Butter and nobody else owns them.
    let mut keeper = Keeper {
        control: is_open(CONTROL_FD).then(|| unsafe { File::from_raw_fd(CONTROL_FD) }),
        output: is_open(1).then(|| unsafe { File::from_raw_fd(1) }),
    };
    set_cloexec(CONTROL_FD);
    set_cloexec(0);
    set_cloexec(1);

    let mut log = match log
        .map(|path| File::options().create(true).append(true).open(path))
        .transpose()
    {
        Ok(log) => log,
        Err(e) => {
            keeper.report(&Report::Failed(format!("could not open the log: {e}")));
            return 1;
        }
    };

    // SAFETY: plain syscall.
    unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) };

    let (master, slave, pts) = match open_pty() {
        Ok(pty) => pty,
        Err(e) => {
            keeper.report(&Report::Failed(format!("could not open a terminal: {e}")));
            return 1;
        }
    };

    let mut command = Command::new(&program);
    command.args(&rest);
    let streams =
        (|| -> io::Result<_> { Ok((slave.try_clone()?, slave.try_clone()?, slave.try_clone()?)) })(
        );
    let child = streams.and_then(|(i, o, e)| {
        command
            .stdin(Stdio::from(i))
            .stdout(Stdio::from(o))
            .stderr(Stdio::from(e));
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(|| {
                // A session of its own, with the terminal as its controlling
                // one, so Ctrl+C and friends reach it as they would in a terminal.
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn()
    });
    drop(slave);
    let child = match child {
        Ok(child) => child,
        Err(e) => {
            let message = format!("could not start {}: {e}", program.to_string_lossy());
            if let Some(log) = &mut log {
                let _ = writeln!(log, "{message}\r");
            }
            keeper.report(&Report::Failed(message));
            return 127;
        }
    };
    let child_pid = child.id() as i32;
    // Reaped below with everything else.
    drop(child);
    keeper.report(&Report::Started {
        pid: child_pid,
        pts,
    });

    set_nonblocking(master.as_raw_fd());
    let mut master = Some(master);
    let mut input = is_open(0).then(|| unsafe { File::from_raw_fd(0) });
    let mut control_buf = Vec::new();
    let mut code = 0;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        // Reap whatever ended: the program, and orphans that fell to us.
        let mut no_children = false;
        loop {
            let mut status = 0;
            // SAFETY: plain syscall.
            let pid = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if pid == child_pid {
                keeper.report(&Report::Exited(status));
                code = exit_code(status);
            } else if pid == 0 {
                break;
            } else if pid < 0 {
                no_children = io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD);
                break;
            }
        }
        if no_children {
            // Whatever is still buffered in the terminal.
            if let Some(m) = &master {
                let mut m = m;
                while let Ok(n @ 1..) = m.read(&mut buf) {
                    keeper.output(&mut log, &buf[..n]);
                }
            }
            return code;
        }

        let mut fds = Vec::with_capacity(3);
        let sources = [
            master.as_ref().map(|f| f.as_raw_fd()),
            input.as_ref().map(|f| f.as_raw_fd()),
            keeper.control.as_ref().map(|f| f.as_raw_fd()),
        ];
        for fd in sources.iter().flatten() {
            fds.push(libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            });
        }
        // Wakes every so often to reap: SIGCHLD is not watched.
        // SAFETY: the array outlives the call.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, 200) };
        if ready <= 0 {
            continue;
        }
        let is_ready = |fd: Option<RawFd>| {
            fd.and_then(|fd| fds.iter().find(|p| p.fd == fd))
                .is_some_and(|p| p.revents != 0)
        };

        if is_ready(sources[0]) {
            match master.as_ref().map(|mut m| m.read(&mut buf)) {
                Some(Ok(n @ 1..)) => keeper.output(&mut log, &buf[..n]),
                Some(Err(e)) if is_transient(&e) => {}
                // EIO: nothing holds the terminal open any more.
                _ => master = None,
            }
        }
        if is_ready(sources[1]) {
            match input.as_mut().map(|i| i.read(&mut buf)) {
                Some(Ok(n @ 1..)) => {
                    if let Some(m) = &master {
                        // Dropped if the program is not reading: Butter must never stall.
                        let _ = (&*m).write(&buf[..n]);
                    }
                }
                Some(Err(e)) if is_transient(&e) => {}
                // Butter is gone; the program keeps running.
                _ => input = None,
            }
        }
        if is_ready(sources[2]) {
            match keeper.control.as_mut().map(|c| c.read(&mut buf)) {
                Some(Ok(n @ 1..)) => {
                    control_buf.extend_from_slice(&buf[..n]);
                    while let Some(end) = control_buf.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = control_buf.drain(..=end).collect();
                        let line = String::from_utf8_lossy(&line);
                        if let (Some(m), Some((rows, cols))) = (&master, parse_resize(&line)) {
                            resize(m.as_raw_fd(), rows, cols);
                        }
                    }
                }
                Some(Err(e)) if is_transient(&e) => {}
                _ => keeper.control = None,
            }
        }
    }
}

struct Keeper {
    control: Option<File>,
    output: Option<File>,
}

impl Keeper {
    fn report(&mut self, report: &Report) {
        if let Some(control) = &mut self.control
            && control.write_all(report.line().as_bytes()).is_err()
        {
            self.control = None;
        }
    }

    /// Program output: always to the log, and to Butter while it listens.
    fn output(&mut self, log: &mut Option<File>, data: &[u8]) {
        if let Some(log) = log {
            let _ = log.write_all(data);
        }
        if let Some(out) = &mut self.output
            && out.write_all(data).is_err()
        {
            self.output = None;
        }
    }
}

fn is_transient(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
    )
}

fn parse_resize(line: &str) -> Option<(u16, u16)> {
    let mut words = line.split_whitespace();
    if words.next()? != "resize" {
        return None;
    }
    Some((words.next()?.parse().ok()?, words.next()?.parse().ok()?))
}

fn exit_code(status: i32) -> i32 {
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        128 + libc::WTERMSIG(status)
    }
}

/// A new pseudo-terminal: its master, its slave, and the slave's path.
fn open_pty() -> io::Result<(File, OwnedFd, String)> {
    let mut master: RawFd = -1;
    let mut slave: RawFd = -1;
    let size = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the out-pointers are valid; the name is read with ttyname below.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            &size,
        )
    };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: openpty returned two new descriptors we now own.
    let (master, slave) = unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    set_cloexec(master.as_raw_fd());
    set_cloexec(slave.as_raw_fd());
    let path = std::fs::read_link(format!("/proc/self/fd/{}", slave.as_raw_fd()))?;
    Ok((master, slave, path.to_string_lossy().into_owned()))
}

fn resize(fd: RawFd, rows: u16, columns: u16) {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: plain ioctl with a valid pointer; the kernel signals SIGWINCH.
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) };
}

fn set_cloexec(fd: RawFd) {
    // SAFETY: plain fcntl calls.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}

pub fn set_nonblocking(fd: RawFd) {
    // SAFETY: plain fcntl calls.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        for report in [
            Report::Started {
                pid: 42,
                pts: "/dev/pts/3".into(),
            },
            Report::Failed("no such file".into()),
            Report::Exited(256),
        ] {
            assert_eq!(Report::parse(report.line().trim_end()), Some(report));
        }
        assert_eq!(parse_resize(&resize_message(30, 100)), Some((30, 100)));
        assert_eq!(parse_resize("resize x 1"), None);
    }
}
