# Contributing

## Development

```sh
cargo test --workspace
cargo build
cargo run -p rufus-linux
```

The desktop has no environment-variable bypass for destructive writes. A live
operation is available only through the installed polkit policy and
`/usr/libexec/rufus-linux-helper`. Use the helper's `--dry-run` mode for request
validation and disposable loop devices for integration tests. Never test on a
disk containing useful data.

## Safety rules

- Keep raw-disk I/O out of the desktop process.
- Revalidate device identity in the helper before the first write.
- Prefer fixed absolute tool paths; never shell out with user-controlled commands.
- Document unavailable Windows-only features honestly (see `docs/CAPABILITIES.md`).

## Style

- Neat, non-redundant modules; one code path per concern.
- Unit tests for planning, safety, image sniffing, and protocol round-trips.
