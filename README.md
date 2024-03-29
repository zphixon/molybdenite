# molybdenite

Async WebSocket library (WIP!)

## TODO

- Compression
- ~~Auto-fragmentation of large sent messages~~
- Correct close state
- Unit tests

## Autobahn

This implementation has been tested for spec-compliance by the [Autobahn test
suite](https://github.com/crossbario/autobahn-testsuite). You can verify for
yourself by running the script in *tests/run_autobahn_container.sh*, then
starting the `autobahn` example. Test case reports will be output into
*tests/reports/clients* or *tests/reports/servers*, containing an *index.html*
with a full summary.