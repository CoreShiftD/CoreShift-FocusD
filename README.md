# CoreShift Foreground Resolution

Android foreground package resolution daemon — event-driven, no polling.

```
coreshift-foreground daemon [--resolver=auto|binder|cgroup]
coreshift-foreground status
coreshift-foreground watch
coreshift-foreground stop
coreshift-foreground restart
```

Communicates over `@coreshift` abstract Unix domain socket.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Resolution Pipeline](docs/RESOLUTION.md)
- [IPC Protocol](docs/IPC_PROTOCOL.md)
- [Configuration](docs/CONFIGURATION.md)

## Credits

Binder observer technique — registering as `IProcessObserver` to gate `getFocusedRootTaskInfo` — by **[sehan64](https://github.com/sehan64)**.

## License

Mozilla Public License 2.0. See [LICENSE](LICENSE).
