# roost-wallpaper

Wallpaper decoding for the compositor: any format the `image` crate reads,
plus JPEG XL through `jxl-oxide`, cover-scaled to an output.

It is a crate of its own so the generic decoding and resizing code is
compiled optimised even in debug builds: the workspace `Cargo.toml` sets
`opt-level = 3` for this crate and its decoders under `profile.dev`.
Unoptimised, GNOME 51's 4096x4096 JPEG XL default wallpapers take tens of
seconds to decode, which would stall nested sessions and proofs.

## Public API (`src/lib.rs`)

- `decode`, `is_jxl`: decode bytes to an image, detect JPEG XL.
- `decode_cover_argb`, `cover_crop`: decode and cover-scale to an output
  size.
- `blur_dim_argb`, `LOCK_BLUR_SIGMA`, `LOCK_BRIGHTNESS`: the lock-screen
  background treatment.
- `card`, `Card`, `Shadow`: rounded, shadowed card rendering.

## Dependents

`roost-compositor`, through its `wallpaper` module.

## Docs

[Settings map](../../docs/settings-map.md) for the GNOME background keys,
[architecture](../../docs/architecture.md).
