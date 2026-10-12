# How Butter works

## How programs are run

Butter never starts `rosemary run` directly. It starts a small keeper first,
a copy of Butter itself run as `butter --supervise`, and the keeper starts
the program:

- The keeper is a child subreaper. Everything the program starts stays
  below it in the process tree, even daemons that detach, such as
  Rosemary's process server. To list a bottle's processes, or to kill them
  all, Butter walks down from its keepers. It never guesses by process name,
  and it signals through a pidfd, so a recycled pid is never hit by mistake.
- The program gets a pseudo-terminal of its own. To the program, its output
  goes to an ordinary terminal, with colours and line editing. The keeper
  copies that output to Butter, which shows it in the Process Manager with
  VTE, and to a log in `logs/`. What you type in the Process Manager is sent
  back to the program. `logs/last-run.log` points at the newest log.
- Programs outlive Butter. If you quit Butter, they keep running and their
  output still goes to the log. Each keeper leaves a record in the bottle's
  `run/` directory, so the next Butter finds them again and follows their
  logs.

In the Process Manager, the stop button asks the focused process to quit
(SIGTERM). Its icon then shows a warning, and pressing it again kills the
process at once (SIGKILL). Other signals are in the menu beside it.

The power button in the header stops the whole bottle:

- **Stop** runs `rosemary server --stop`, which asks the bottle's Rosemary
  server to shut down cleanly.
- **Force Stop** sends SIGKILL to every process Butter started in the bottle,
  and to everything those processes started.

## How an official build is installed

Each Rosemary release attaches a Steam compatibility tool,
`rosemary-steam-tool-<version>-<arch>.tar.gz`. It contains the same
self-contained bundle as the release's AppImage: Rosemary, the SDL 3, OpenAL
and other libraries it needs (often newer than the host's), and a glibc for
hosts whose own is too old. It runs outside Steam too. The only thing it
takes from the host is the OpenGL or Vulkan driver, and the Flatpak gets that
from its runtime.

1. Butter downloads the archive for this computer's architecture.
2. It checks the archive's SHA-256 against the digest GitHub reports, or
   against the release's `SHA256SUMS`. Nothing is installed if they differ.
3. It unpacks the tool into `runners/rosemary-<tag>-official/Rosemary/`.
4. It writes `bin/rosemary`, a launcher that does what the tool's
   `rosemary-steam` script does: it undoes the Steam runtime's environment
   (if Butter was started from Steam), then runs the bundle's
   `bin/rosemary`. That launcher picks the host's or the bundled glibc and
   puts the bundled libraries first. Butter runs it like any other runner and
   sets the prefix itself.

## How a runner is built from source

1. Butter downloads the tag's source archive from GitHub.
2. It reads the Rust version the release needs, from `rust-version` in its
   `Cargo.toml` or from its README. It then picks a toolchain that is new
   enough. If Butter's own toolchain is too old, Butter updates it.
3. It runs `cargo build --locked --bin rosemary` with the `release` profile, or
   with Rosemary's `fullnative` profile if you choose **Optimized for this
   CPU** in Preferences. All builds share one target directory, so a later
   release compiles only what changed.
4. It installs the binary as `runners/rosemary-<tag>/bin/rosemary`.

Release information comes from the GitHub API. The list is cached for 15
minutes. If GitHub can't be reached, Butter shows the list it fetched last.
