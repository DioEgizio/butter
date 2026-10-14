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

## How applications are found

A bottle's page lists the `.app` bundles in its `/Applications`, in the
folders inside it (such as `/Applications/Utilities`), and in each user's
`~/Applications`. Butter watches those folders, so an app an installer puts
there appears on its own. Programs added by hand are listed with them; adding
or customizing an installed app keeps its own arguments and backend.

For each bundle, Butter reads `Contents/Info.plist`, in XML or binary form:

- The name is `CFBundleDisplayName`, then `CFBundleName`, then the folder's.
- The icon is the `.icns` file `CFBundleIconFile` (or `CFBundleIconName`)
  names in `Contents/Resources`. Butter takes its largest image up to
  512 pixels, PNG or the older run-length encoded kind, and caches it as a
  PNG.
- The links it opens are the schemes in `CFBundleURLTypes`.
- The files it opens are its `CFBundleDocumentTypes`. Their MIME types come
  from `CFBundleTypeMIMETypes`, from the types the app declares itself
  (`UTExportedTypeDeclarations` and `UTImportedTypeDeclarations`), from
  Apple's common types, and from `CFBundleTypeExtensions` through the shared
  MIME database. Abstract types, such as `public.data`, are left out, and so
  are types the app only knows of (role `None`).

## How shortcuts work

**Create Shortcut** asks the desktop, through the
[Dynamic Launcher portal](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.DynamicLauncher.html),
to add the application to its applications menu. The desktop shows a dialog
where you confirm it, and can rename it. The desktop entry:

- runs `butter --run <bottle> [--backend B] [--arg A]... <app> %U`;
- lists the app's types and `x-scheme-handler/<scheme>` for its schemes in
  `MimeType=`, so the desktop offers it for those files and links;
- gets a category from `LSApplicationCategoryType`, such as Game.

`butter --run` starts the app the way its Run button does, so it shows up in
the Process Manager. If Butter is open, the open Butter starts it. If not,
Butter starts it without opening a window, and quits once it ends. A window
only opens if something went wrong.

The links and files the desktop passes are given to the app as URLs, which
Rosemary hands to `-[NSApplicationDelegate application:openURLs:]`. Links
are passed unchanged. A file inside the bottle becomes a `file:` URL of its
path there, and any other file one under `/Volumes/Host`, where the bottle
sees the host's files.

Shortcuts are removed from an application's menu, and all of a bottle's are
removed when it is deleted. Outside the Flatpak, if the portal is missing,
Butter writes the desktop entry into `~/.local/share/applications` itself.

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
