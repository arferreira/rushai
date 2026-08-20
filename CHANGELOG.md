# Changelog

## 0.1.0 - 2026-08-20

First publish. Workspace with four crates:

- rushai-protocol: op, event, and message part wire types
- rushai-config: layered config discovery, deep merge, env mapping
- rushai-core: session store on a dedicated SQLite thread
- rushai: the rush binary with config and sessions subcommands

Not usable as an assistant yet. This release claims the name and sets the
foundation; the provider layer is next.
