# video-stress

A source-agnostic monochrome video playback harness. It measures the
workspace renderer, not a video decoder.

## Why the clip is not in this repository

Every frame is decoded ahead of time into a packed 1-bit-per-pixel file, so
playback never touches the disk or a codec. Two consequences follow:

* The source video is an *input to a build step*, not a source file. The
  tools read any file `ffmpeg` can decode, so the clip is your choice.
* Both the source video and the `.mfp` pack are large binaries. Git keeps
  every byte of them forever. `assets/` and `*.mfp` are therefore ignored.

Supply your own clip. Put it anywhere; `assets/` is ignored and convenient.

## Prepare a pack

```
cargo run -p video-stress --bin video-prepare -- --input assets/clip.mp4
```

This writes `frames.mfp` at 480x360 and 60 fps. Use `--width`, `--height`,
`--fps` and `--threshold` to change that, and `--output` to name the pack.
Run the tool with `--help` for the full list.

`ffmpeg` must be on `PATH`.

## Play a pack

Both players read a pack and report renderer throughput:

```
cargo run -p video-stress --bin video-terminal  -- --pack frames.mfp
cargo run -p video-stress --bin video-wallpaper -- --pack frames.mfp
```

`video-terminal` draws into a terminal with half, quadrant, or sextant
blocks (`--cells`). `video-wallpaper` draws to a Wayland surface and takes
`--fit`. Both accept `--frames` and `--seconds` to bound a measurement run.

## Check a font

`video-fontcheck` reports which block glyphs a font actually covers, which
decides whether `--cells sextant` can be used with it.
