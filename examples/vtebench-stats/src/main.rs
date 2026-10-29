//! Compares two vtebench `--dat` files, one row per benchmark.
//!
//! Usage: `vtebench-stats first.dat second.dat`. Each file is labelled by its
//! file stem. Percentiles are nearest-rank on the sorted samples. A `_` in a
//! file marks a benchmark that had already finished and is skipped.

use std::{env, fs, path::Path};

struct Column {
    name: String,
    samples: Vec<u32>,
}

impl Column {
    fn sorted(&self) -> Vec<u32> {
        let mut sorted = self.samples.clone();
        sorted.sort_unstable();
        sorted
    }
}

struct Summary {
    count: usize,
    median: u32,
    p95: u32,
    max: u32,
}

impl Summary {
    fn of(column: &Column) -> Self {
        let sorted = column.sorted();
        Self {
            count: sorted.len(),
            median: Self::percentile(&sorted, 50),
            p95: Self::percentile(&sorted, 95),
            max: sorted[sorted.len() - 1],
        }
    }

    const fn percentile(sorted: &[u32], percent: usize) -> u32 {
        sorted[((sorted.len() - 1) * percent + 50) / 100]
    }
}

struct DatFile {
    label: String,
    columns: Vec<Column>,
}

impl DatFile {
    fn load(path: &str) -> Self {
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
        let mut lines = text.lines();
        let header = lines.next().expect("empty file");
        let mut columns: Vec<Column> = header
            .split_whitespace()
            .map(|name| Column {
                name: name.to_owned(),
                samples: Vec::new(),
            })
            .collect();
        for line in lines {
            for (column, token) in columns.iter_mut().zip(line.split_whitespace()) {
                if let Ok(value) = token.parse() {
                    column.samples.push(value);
                }
            }
        }
        let label = Path::new(path)
            .file_stem()
            .map_or_else(|| path.to_owned(), |s| s.to_string_lossy().into_owned());
        Self { label, columns }
    }

    fn column(&self, name: &str) -> &Column {
        self.columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{}: no column {name}", self.label))
    }
}

struct Report {
    first: DatFile,
    second: DatFile,
}

impl Report {
    fn from_args() -> Self {
        let args: Vec<String> = env::args().skip(1).collect();
        assert!(args.len() == 2, "usage: vtebench-stats first.dat second.dat");
        Self {
            first: DatFile::load(&args[0]),
            second: DatFile::load(&args[1]),
        }
    }

    fn print(&self) {
        let (a, b) = (&self.first.label, &self.second.label);
        println!(
            "| benchmark | {a} n | {a} median | {a} p95 | {a} max | {b} n | {b} median | {b} p95 | {b} max | {b} / {a} |"
        );
        println!("|---|---|---|---|---|---|---|---|---|---|");
        for column in &self.first.columns {
            let x = Summary::of(column);
            let y = Summary::of(self.second.column(&column.name));
            println!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.2}x |",
                column.name,
                x.count,
                x.median,
                x.p95,
                x.max,
                y.count,
                y.median,
                y.p95,
                y.max,
                f64::from(y.median) / f64::from(x.median),
            );
        }
    }
}

fn main() {
    Report::from_args().print();
}
