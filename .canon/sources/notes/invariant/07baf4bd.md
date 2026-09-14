# macOS lld linker (per .cargo/config.toml) needs explicit @loader_path/@executable_path rpath rustflags; without them sovereign-cli can't…

macOS lld linker (per .cargo/config.toml) needs explicit @loader_path/@executable_path rpath rustflags; without them sovereign-cli can't load co-located libggml-*.dylib and crashes at startup

`.cargo/config.toml` sets `-fuse-ld=lld` for both apple-darwin triples (introduced in 76d4c4a "speed up all build stuffs", 2026-05-17). lld does NOT inject the implicit `@loader_path` rpath that Apple's ld64 adds by default. Result: any binary that links to dylibs shipped next to it (llama-cpp-sys-2 drops `libggml-base.0.dylib` etc. into `target/<profile>/`) crashes at startup:

```
dyld[…]: Library not loaded: @rpath/libggml-base.0.dylib
  Reason: no LC_RPATH's found
```

Why: lld's macOS backend treats rpath as fully explicit, no defaults. ld64 silently adds `@loader_path`. Switching linkers without compensating rpath flags breaks any locally-linked dylib resolution.

How to apply:
- The fix already in tree is explicit rustflags in both apple-darwin blocks: `-Wl,-rpath,@loader_path` + `-Wl,-rpath,@executable_path`. Don't strip these.
- If a binary on disk still has no LC_RPATH (e.g. built before the flags landed): patch with `install_name_tool -add_rpath @loader_path <bin> && install_name_tool -add_rpath @executable_path <bin>`. Then plan a rebuild so future builds bake it in.
- Check rpath with `otool -l <bin> | grep -A2 LC_RPATH`.
- If reverting to system ld64 someday: the explicit rpath flags become harmless dupes, leave them.
- Same trap applies to any future binary added to this workspace that links co-located dylibs — the rustflags already cover them, but be aware when debugging dyld errors on fresh targets.

Related: [[reference-daemon-restart-lwcr]] for daemon lifecycle once binary loads.


## Index overflow (moved from MEMORY.md 2026-07-07 compaction)

- [macOS lld needs explicit rpath rustflags (2026-05-17)](invariant_macos_lld_rpath.md) — `.cargo/config.toml` uses lld for apple-darwin; lld omits ld64's default `@loader_path` rpath, so co-located `libggml-*.dylib` fails to load. Mandatory `-Wl,-rpath,@loader_path` + `@executable_path` rustflags in tree; don't strip.

---
