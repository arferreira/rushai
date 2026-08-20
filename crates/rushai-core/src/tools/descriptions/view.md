Read a file from the workspace. Returns numbered lines.

Use `offset` (1-based line) and `limit` to read a window of a large file.
Output is capped; a note at the end tells you when it was truncated.
Binary files are rejected.
