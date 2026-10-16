//! Runs `butter --supervise` the way Butter does, and checks the program gets
//! a terminal, its output and input are relayed, and processes that detach
//! stay below the keeper until they end.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn children_of(pid: u32) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(close) = stat.rfind(')') else {
            continue;
        };
        let comm = stat[stat.find('(').unwrap() + 1..close].to_string();
        let fields: Vec<&str> = stat[close + 1..].split_whitespace().collect();
        if fields[1] == pid.to_string() && fields[0] != "Z" {
            found.push((stat[..stat.find(' ').unwrap()].parse().unwrap(), comm));
        }
    }
    found
}

#[test]
fn keeps_programs_on_a_terminal_of_their_own() {
    let dir = std::env::temp_dir().join(format!("butter-supervisor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("run.log");

    // A program that checks its terminal, leaves a daemon behind, echoes a
    // line of input and fails.
    let script = "test -t 0 && test -t 1 && echo on-a-tty; \
                  trap '' HUP; setsid sleep 30 >/dev/null 2>&1 & \
                  read line; echo got-$line; exit 3";
    let (ours, theirs) = UnixStream::pair().unwrap();
    let fd = theirs.as_raw_fd();
    let mut keeper = Command::new(env!("CARGO_BIN_EXE_butter"));
    keeper
        .args(["--supervise", "--log"])
        .arg(&log)
        .args(["--", "sh", "-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    unsafe {
        keeper.pre_exec(move || {
            if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut keeper = keeper.spawn().unwrap();
    drop(theirs);
    let mut control = BufReader::new(ours);

    let mut line = String::new();
    control.read_line(&mut line).unwrap();
    let words: Vec<&str> = line.split_whitespace().collect();
    assert_eq!(words[0], "started", "{line}");
    assert!(words[2].starts_with("/dev/pts/"), "{line}");

    let mut input = keeper.stdin.take().unwrap();
    input.write_all(b"hello\n").unwrap();

    line.clear();
    control.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), format!("exited {}", 3 << 8));

    // The daemon fell to the keeper rather than to init.
    let deadline = Instant::now() + Duration::from_secs(5);
    let daemon = loop {
        if let Some(found) = children_of(keeper.id())
            .into_iter()
            .find(|(_, c)| c == "sleep")
        {
            break found.0;
        }
        assert!(
            Instant::now() < deadline,
            "the daemon is not below the keeper"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        keeper.try_wait().unwrap().is_none(),
        "the keeper waits for it"
    );
    unsafe { libc::kill(daemon as i32, libc::SIGKILL) };
    assert_eq!(keeper.wait().unwrap().code(), Some(3));

    let mut output = String::new();
    keeper
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(output.contains("on-a-tty"), "{output:?}");
    assert!(output.contains("got-hello"), "{output:?}");
    let logged = std::fs::read_to_string(&log).unwrap();
    assert!(logged.contains("got-hello"), "{logged:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn reports_programs_that_cannot_start() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let fd = theirs.as_raw_fd();
    let mut keeper = Command::new(env!("CARGO_BIN_EXE_butter"));
    keeper
        .args(["--supervise", "--", "/nonexistent/program"])
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    unsafe {
        keeper.pre_exec(move || {
            libc::dup2(fd, 3);
            Ok(())
        });
    }
    let mut keeper = keeper.spawn().unwrap();
    drop(theirs);
    let mut line = String::new();
    BufReader::new(ours).read_line(&mut line).unwrap();
    assert!(line.starts_with("failed "), "{line}");
    assert_eq!(keeper.wait().unwrap().code(), Some(127));
}

#[test]
fn programs_left_behind_survive_the_program_ending() {
    // Like a self-updater: the program starts a helper in its own process
    // group, without detaching it or ignoring SIGHUP, and quits. The helper
    // must not be hung up when the program ends.
    let dir = std::env::temp_dir().join(format!("butter-updater-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let marker = dir.join("updated");
    let script = format!(
        "(sleep 0.5; touch '{}') </dev/null >/dev/null 2>&1 & exit 0",
        marker.display()
    );
    let (ours, theirs) = UnixStream::pair().unwrap();
    let fd = theirs.as_raw_fd();
    let mut keeper = Command::new(env!("CARGO_BIN_EXE_butter"));
    keeper
        .args(["--supervise", "--", "sh", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null());
    unsafe {
        keeper.pre_exec(move || {
            libc::dup2(fd, 3);
            // As Butter starts it: out of the caller's session.
            libc::setsid();
            Ok(())
        });
    }
    let mut keeper = keeper.spawn().unwrap();
    drop(theirs);
    let mut control = BufReader::new(ours);
    let mut line = String::new();
    control.read_line(&mut line).unwrap();
    assert!(line.starts_with("started "), "{line}");
    line.clear();
    control.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "exited 0");

    assert_eq!(keeper.wait().unwrap().code(), Some(0));
    assert!(
        marker.exists(),
        "the helper was killed when the program ended"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
