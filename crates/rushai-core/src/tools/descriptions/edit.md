Replace an exact substring in a file.

`old_string` must match the current file contents exactly, including
indentation, and must be unique unless `replace_all` is set. Include enough
surrounding context to make the match unambiguous. The file's existing line
endings are preserved.
