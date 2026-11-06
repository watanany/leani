# Configuration

Put the configuration file at `~/.config/leani/config.toml`.
leani works without it.

## Environments

You can give a name to a combination of a Lake project and imports and register it as an **environment**.

```toml
default = "main"

[env.main]
project = "~/src/my-project"
imports = ["MyProject"]

[env.math]
project = "~/src/my-project"
imports = ["Mathlib"]
prompt = "λ∀> "
```

| Key                  | Description                                                           |
| -------------------- | --------------------------------------------------------------------- |
| `default`            | Name of the environment used when leani starts without options        |
| `engine`             | Directory of an engine you built yourself (leanprover-community/repl) |
| `env.<名前>.project` | Directory of the Lake project                                         |
| `env.<名前>.imports` | Array of modules to import                                            |
| `env.<名前>.prompt`  | Prompt string                                                         |
| `env.<名前>.engine`  | Directory of the engine used only in that environment                 |

When you specify the engine yourself, `env.<名前>.engine`, `engine`, and the environment variable `LEANI_ENGINE` take priority in that order.
If you do not specify one, leani builds the engine automatically.

## How the environment to start in is chosen

When you specify a name, as in `leani -e math`, leani starts in that environment.
When you give `-p` or `-i` without `-e`, leani does not use the environments in the configuration and starts with the given project and imports.
With no options, leani starts in the `default` environment. If there is no `default`, leani starts in the Lake project that contains the current directory.
In the REPL, `:env <名前>` switches the environment.

## init file

Declarations written in `~/.config/leani/init.lean` run every time leani starts.
They remain after `:reset` and `:l`.

## Environment variables

| Environment variable | Description                                                                                    |
| -------------------- | ---------------------------------------------------------------------------------------------- |
| `LEANI_CONFIG`       | Path of the configuration file                                                                 |
| `LEANI_INIT`         | Path of the init file                                                                          |
| `LEANI_HISTORY`      | Path of the history file                                                                       |
| `LEANI_ENGINE`       | Directory of an engine you built yourself                                                      |
| `LEANI_NO_SETUP`     | If set to a non-empty value, leani does not build the engine automatically                     |
| `LEANI_LOOGLE`       | URL that `:loogle` sends queries to                                                            |
| `MANPAGER`           | Pager that shows the `:abbrev` list. If it is not set, leani uses `PAGER`                      |
| `PAGER`              | Pager used when `MANPAGER` is not set. If neither is set, leani uses `less`                    |
| `FORCE_COLOR`        | If set to a non-empty value, leani uses colors even without a terminal. `0` also turns them on |

## File locations

| File             | Location                                   |
| ---------------- | ------------------------------------------ |
| Configuration    | `~/.config/leani/config.toml`              |
| init             | `~/.config/leani/init.lean`                |
| History          | `~/.local/state/leani/history`             |
| Engine           | `~/.local/state/leani/engine/<バージョン>` |
| `lake env` cache | `~/.local/state/leani/lake-env/`           |

If `XDG_CONFIG_HOME` and `XDG_STATE_HOME` are set, leani uses those directories instead of `~/.config` and `~/.local/state`.
`leani -V` shows the actual locations.
