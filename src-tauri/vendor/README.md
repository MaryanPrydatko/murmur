# Vendored crates

`handy-cpal` 0.18.2 and `handy-recorder` 0.1.1 as published on crates.io, with
one patch, wired in through `[patch.crates-io]` in `../Cargo.toml`.

The patch lets a meeting record what the Mac plays:

- `handy-cpal`: `Device::with_loopback()` opens input streams as a Core Audio
  process tap on an output device, also when that device has an input side,
  and without changing the device's format.
- `handy-recorder`: the device id `SYSTEM_OUTPUT_DEVICE` opens the default
  output that way, and holds an output stream on it so the tap delivers audio
  from the start. That stream plays a constant too small to hear or to reach
  a 16-bit recording; an allowed tap hears it, so a track of only zeros means
  macOS withheld the audio.

Everything the patch adds is macOS only. To see it, diff each folder against
the crate of the same version from crates.io, or read the commit after the one
that added the folders unchanged.
