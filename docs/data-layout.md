# Where things are

| What | Where |
| --- | --- |
| Runners | `$XDG_DATA_HOME/butter/runners/<id>/` (`runner.toml`, `bin/rosemary`, and `Rosemary/` for an official build) |
| Bottles | `$XDG_DATA_HOME/butter/bottles/<id>/` (`bottle.toml`, `prefix/`, `logs/`, and `run/` while programs run) |
| Butter's Rust toolchain | `$XDG_DATA_HOME/butter/toolchain/` |
| Build cache and release list | `$XDG_CACHE_HOME/butter/` |
| Preferences | `$XDG_CONFIG_HOME/butter/settings.toml` |
| Onboarding answers (Flatpak) | `$XDG_CONFIG_HOME/butter/permissions.toml` |

In the Flatpak, these are under `~/.var/app/io.github.microlift.Butter/`.

A bottle's `prefix/` directory is an ordinary Rosemary prefix. Butter runs
programs with `ROSEMARY_PREFIX` pointing at it, and manages it with
`rosemary prefix create|info|verify|repair|reset-system|update`. Its settings
become Rosemary's environment variables, such as `ROSEMARY_DECORATIONS`,
`ROSEMARY_MENU` and `ROSEMARY_SWAP_INTERVAL`. See Rosemary's
[usage guide](https://github.com/microlift/rosemary/blob/main/docs/usage.md).
