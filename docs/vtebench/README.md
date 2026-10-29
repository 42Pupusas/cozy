# vtebench: cozy and Ghostty

Results from running [vtebench](https://github.com/alacritty/vtebench) in the
cozy terminal and in Ghostty on the same machine. The raw files are
[`cozy.dat`](cozy.dat) and [`ghostty.dat`](ghostty.dat).

## What vtebench measures

vtebench writes a fixed payload to the terminal's pty and times how long the
write blocks. It records milliseconds per sample, repeats until `--max-secs`
(default 10) runs out, and writes one column per benchmark.

That time is how fast the terminal drains the pty. It is not how fast the
terminal draws. A terminal that reads quickly and presents fewer frames would
score well. Read the numbers as parse-and-apply throughput.

Cozy has tests that check the bytes written to the pty reach the grid intact:
`benches/tty/tests/delivery.rs` and `benches/tty/tests/pty_delivery.rs`.

## Results

Milliseconds per sample. Lower is faster. `n` is the sample count; a faster
terminal fits more samples into the fixed time. The last column is Ghostty's
median divided by cozy's.

| benchmark | cozy n | cozy median | cozy p95 | cozy max | ghostty n | ghostty median | ghostty p95 | ghostty max | ghostty / cozy |
|---|---|---|---|---|---|---|---|---|---|
| cursor_motion | 1001 | 10 | 10 | 12 | 295 | 34 | 37 | 40 | 3.40x |
| dense_cells | 231 | 43 | 47 | 50 | 67 | 151 | 164 | 173 | 3.51x |
| light_cells | 3039 | 3 | 4 | 6 | 502 | 19 | 23 | 26 | 6.33x |
| medium_cells | 974 | 10 | 11 | 15 | 389 | 25 | 30 | 33 | 2.50x |
| scrolling | 43 | 201 | 210 | 212 | 39 | 216 | 227 | 230 | 1.07x |
| scrolling_bottom_region | 49 | 203 | 218 | 221 | 47 | 215 | 231 | 234 | 1.06x |
| scrolling_bottom_small_region | 48 | 208 | 223 | 233 | 43 | 234 | 245 | 264 | 1.12x |
| scrolling_fullscreen | 192 | 9 | 12 | 13 | 144 | 21 | 27 | 29 | 2.33x |
| scrolling_top_region | 45 | 221 | 240 | 240 | 43 | 232 | 245 | 250 | 1.05x |
| scrolling_top_small_region | 46 | 219 | 231 | 234 | 44 | 230 | 246 | 250 | 1.05x |
| sync_medium_cells | 828 | 11 | 14 | 17 | 355 | 28 | 32 | 40 | 2.55x |
| unicode | 1170 | 8 | 10 | 13 | 236 | 16 | 19 | 22517 | 2.00x |

Regenerate the table with:

```sh
cargo run --release -p vtebench-stats -- docs/vtebench/cozy.dat docs/vtebench/ghostty.dat
```

## Reading the results

- On the cell, cursor, fullscreen-scroll and unicode benchmarks, cozy's 95th
  percentile is below Ghostty's median. The two distributions do not overlap.
- On the five other scrolling benchmarks both terminals take about 200 ms and
  cozy leads by 5 to 12%. There are only 39 to 49 samples per benchmark and a
  spread of 6 to 10 ms, so treat these as close to a tie.
- Ghostty's `unicode` column contains one sample of 22,517 ms. The other 235
  samples are 15 to 27 ms. The outlier is left in the data. Use the median;
  the mean is meaningless for that column.

## Setup

| | |
|---|---|
| CPU | AMD Ryzen 7 7730U |
| Compositor | niri |
| Cozy | release build of commit `8f284f5` |
| Ghostty | 1.3.1-2, Arch Linux package (`pacman -S ghostty`) |
| vtebench | benchmarks from commit `ead80032e57dee2e75f0b51f2ea67528647d9944` (see below) |
| Window | both terminals full screen |

Command for the Ghostty run. The cozy command was not recorded; it differed at most in the output file:

```sh
vtebench -b /tmp/vtebench/benchmarks --dat /tmp/ghostty.dat
```

## Limits of this comparison

- The window size in columns and rows was not recorded. The benchmarks size
  their output from `tput cols` and `tput lines`, so each terminal received a
  payload that depended on its own window. Both were full screen, but the cell
  grid may differ between them. The display resolution and refresh rate were
  not recorded either.
- `cozy.dat` was saved as `cozy2.dat`, the second cozy run; the first is not
  included. Ghostty has one run. There is no run-to-run variance beyond the
  spread within a run.
- The `/tmp/vtebench` checkout was deleted and cloned again between the
  earlier cozy run and these two. The `vtebench` binary and the benchmark
  scripts may therefore come from different commits; this was not checked.
- The cozy binary is a release build from HEAD (`8f284f5`), per the author. It
  was not rebuilt from a clean checkout for this comparison.
- Ghostty's configuration was not recorded.
- vtebench measures pty drain time, as described above, not rendered frames.
