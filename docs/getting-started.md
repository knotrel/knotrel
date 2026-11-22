---
layout: default
title: Getting started
---
# Getting started

Knotrel answers connectivity queries on a changing undirected graph. The server keeps its graph in memory; it does not persist data or provide authentication.

## Try a graph locally

1. Download the server for your platform from [release v0.9.0](https://github.com/knotrel/knotrel/releases/tag/v0.9.0) and [verify the checksum](https://github.com/knotrel/knotrel#installing-a-release).
2. Clone the [repository](https://github.com/knotrel/knotrel) for the example. Install Python 3.9 or newer.
3. From the repository root, run:

```sh
python3 examples/quickstart/network_demo.py /absolute/path/to/knotrel-server
```

On Windows use `py` and the path to `knotrel-server.exe`. The example starts its own local server, creates two paths, takes nodes offline and checks connectivity after each change. It stops the server when done. See the [complete quickstart](https://github.com/knotrel/knotrel#quickstart-a-failure-and-a-recovery) for expected output.

## Open the console

The browser console is distributed separately from the server. Follow the [console guide](console.md) to start it, enable optional inspection and explore your graph.

## Next steps

- Read the [architecture](architecture-site.md) and [HTTP API](http-api.md).
- For an external source, read the [integration contract](integration-contract.md).
- Before deploying, follow the [deployment guide](deployment.md) and place Knotrel behind an appropriate trusted boundary.
- For the Rust library, see [knotrel-core on docs.rs](https://docs.rs/knotrel-core/latest/knotrel_core/).
