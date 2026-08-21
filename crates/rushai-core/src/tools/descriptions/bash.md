Run a shell command in the workspace and return its combined output.

Runs through your shell with a timeout. Long or wandering output is capped.
A short list of destructive commands is refused outright; that check is a
tripwire, not a sandbox, so still review what you run. Prefer the dedicated
file tools over shell equivalents when one exists.
