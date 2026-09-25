# steam-protobuf-dumper

Extracts embedded [Protocol Buffer](https://protobuf.dev/) definitions from Steam client binaries. A Rust port of SteamKit's [ProtobufDumper](https://github.com/SteamRE/SteamKit/tree/master/Resources/ProtobufDumper).

## How it works

Steam's native binaries (`steamclient.so`, `steamui.so`) embed serialized `FileDescriptorProto` blobs. This tool scans the raw bytes for protobuf markers, parses the descriptors, resolves cross-file dependencies, and reconstructs human-readable `.proto` source files.

## Why does this exist?

You might be wondering: why make a Rust version of this instead of just using the original tool?

When I started this port, compiling the original tool to AOT wasn't really an option without modifying some of its dependencies and libraries. I also didn't want to have the C#/.NET runtime installed on my machine just to run a small utility like this (average Linux user LMAO).

Instead of going through the trouble of making the original tool AOT-compatible, I decided to just port it to Rust. I generally prefer AOT-compiled software over JIT, and, for reasons I can't really explain, I don't particularly like C# either.

So yeah, that's pretty much it.

## Usage

```bash
steam-protobuf-dumper steam_bins/steamclient.so steam_bins/steamui.so protos/
```

Pass `--debug` for verbose logging:

```bash
steam-protobuf-dumper --debug steam_bins/steamclient.so steam_bins/steamui.so protos/
```

## Releases

If you don't want to build the project from source, you can download the latest pre-built binaries from the [**Releases**](https://github.com/enXov/steam-protobuf-dumper/releases/latest) page.

## Building

```bash
cargo build --release
```

The binary is at `target/release/steam-protobuf-dumper` (~2 MB, stripped).

## Credits

- [SteamRE/SteamKit](https://github.com/SteamRE/SteamKit/tree/master) - The original SteamKit library
- [ProtobufDumper](https://github.com/SteamRE/SteamKit/tree/master/Resources/ProtobufDumper/ProtobufDumper) - The C# tool this project is ported from
- [SteamTracking/SteamTracking](https://github.com/SteamTracking/SteamTracking) - Steam client tracking and extraction pipeline
- [SteamTracking/Protobufs](https://github.com/SteamTracking/Protobufs) - Aggregated protobuf definitions from Steam
- [SteamTracking/GameTracking](https://github.com/SteamTracking/GameTracking) - Game-specific tracking and protobuf extraction

## License

[MIT](LICENSE)
