# Linux string catalog

The desktop window, panel menu, command-line app and terminal helper share
the catalog in `catalog/`. The compiler emits Rust resources for the app
and supplies the strings used to generate desktop and package metadata.
Production includes English (United States).

Each process resolves its message locale once. The window resolves it at startup.
`LC_ALL`, `LC_MESSAGES` and `LANG` determine the base locale, in that order.
`LANGUAGE` supplies a colon-separated preference list for locales other than
`C` and `POSIX`. Fluent tries the listed languages, then the base locale, then `en-US`. The terminal helper inherits its
parent's environment. The service uses the systemd user manager's
environment. The graphical autostart entry imports the session's locale
variables into that manager before it asks systemd to start the service.

GTK reads the session locale for its own widgets and dialogs. The window
sets its layout direction from the app's selected language before building
its widgets. The window's date and number formatters use the session's regional
settings. The source summary keeps its existing English number grouping.

Desktop launchers and KWin metadata gain locale-qualified keys when
additional locales are enabled.
GNOME Shell extension metadata and package descriptions have a single
source-language value. The user service's description is rendered when
the unit is written. Command names, flags, input tokens, journal mark
words, wire fields and diagnostic logs are stable values. Clap, systemd
and the desktop environment supply their own messages.

## Editing and regenerating

Edit the source Fluent file and its message sidecar. Use a whole sentence
with named arguments when the app supplies a value. See the [compiler
documentation](https://github.com/solpbc/solstone-journal/tree/main/tools/i18n)
for source locks, sidecar revisions, history and retirement.

Build the compiler revision recorded in `compiler.json` with its locked
dependencies. Regenerate the outputs and run the drift check:

```sh
I18N_COMPILER=/path/to/i18n-catalog python3 scripts/i18n.py
make check-i18n
```

`make ci` runs the same drift check offline. Commit catalog changes with
the regenerated files. Adding a language requires an approved translation,
registry membership, an updated compiler pin and regenerated resources.

## Draft locales

`es-419` is registered in the pinned locale registry as a draft locale (`development` released, `production` withheld). Draft translations live in `catalog/translations/es-419.ftl` (or sharded files under `catalog/translations/es-419/`), and production builds exclude them. Unapproved legal and statutory messages stay absent from draft translation files and fall back to English in development builds. When editing English prose, run `i18n-catalog lock` to refresh `source-lock.json`, copy each changed `source` hash into its sidecar `source_revision`, and do not pass `--acknowledge es-419`: leaving the prior acknowledgment untouched marks edited draft entries stale until a translator updates them.
