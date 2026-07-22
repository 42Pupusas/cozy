//! Isolated procfs parser benchmarks. These intentionally exclude filesystem
//! latency so representation/allocation changes are measurable.

use statusbar::monitor::MonitorReader;

const PROC_STAT: &str = "42 (worker with spaces) S 1 2 3 4 5 6 7 8 9 10 123 456 13 14 15 16 17 18 19 20 321";
const PROC_CPU: &str = "cpu  123 45 67 890 12 3 4 5 6 7\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
const PROC_NET: &str = "Inter-| Receive | Transmit\n face |bytes packets errs drop fifo frame compressed multicast |bytes packets errs drop fifo colls carrier compressed\neth0: 12345 2 0 0 0 0 0 0 67890 3 0 0 0 0 0 0\n";

#[divan::bench]
fn process_stat_streaming(bencher: divan::Bencher) {
    bencher.bench(|| divan::black_box(MonitorReader::parse_process_stat(divan::black_box(PROC_STAT))));
}

#[divan::bench]
fn process_stat_byte(bencher: divan::Bencher) {
    bencher.bench(|| divan::black_box(MonitorReader::parse_process_stat(divan::black_box(PROC_STAT))));
}

#[divan::bench]
fn process_stat_allocating(bencher: divan::Bencher) {
    bencher.bench(|| {
        let close = PROC_STAT.rfind(')').unwrap();
        let name = PROC_STAT.get(..close).unwrap().split_once('(').unwrap().1.to_owned();
        let fields: Vec<&str> = PROC_STAT.get(close + 1..).unwrap().split_whitespace().collect();
        let user: u64 = fields[11].parse().unwrap();
        let system: u64 = fields[12].parse().unwrap();
        let resident: u64 = fields[21].parse().unwrap();
        divan::black_box((name, user + system, resident))
    });
}

#[divan::bench]
fn cpu_times_byte_parser(bencher: divan::Bencher) {
    bencher.bench(|| {
        divan::black_box(MonitorReader::parse_cpu_times_for_bench(divan::black_box(PROC_CPU)))
    });
}

#[divan::bench]
fn cpu_line_streaming(bencher: divan::Bencher) {
    bencher.bench(|| {
        let mut total = 0u64;
        let mut idle = 0u64;
        let mut iowait = 0u64;
        for (index, field) in PROC_CPU.lines().next().unwrap().split_whitespace().skip(1).enumerate() {
            let value: u64 = field.parse().unwrap();
            total += value;
            match index { 3 => idle = value, 4 => iowait = value, _ => {} }
        }
        divan::black_box((total, idle + iowait))
    });
}

#[divan::bench]
fn cpu_line_allocating(bencher: divan::Bencher) {
    bencher.bench(|| {
        let values: Vec<u64> = PROC_CPU.lines().next().unwrap().split_whitespace().skip(1).map(|field| field.parse().unwrap()).collect();
        divan::black_box((values.iter().sum::<u64>(), values[3] + values[4]))
    });
}

#[divan::bench]
fn network_line_streaming(bencher: divan::Bencher) {
    bencher.bench(|| {
        let data = PROC_NET.lines().nth(2).unwrap().split_once(':').unwrap().1;
        let mut rx = 0;
        let mut tx = 0;
        for (index, field) in data.split_whitespace().enumerate() {
            let value: u64 = field.parse().unwrap();
            match index { 0 => rx = value, 8 => tx = value, _ => {} }
        }
        divan::black_box((rx, tx))
    });
}

#[divan::bench]
fn network_line_allocating(bencher: divan::Bencher) {
    bencher.bench(|| {
        let data = PROC_NET.lines().nth(2).unwrap().split_once(':').unwrap().1;
        let fields: Vec<u64> = data.split_whitespace().map(|field| field.parse().unwrap()).collect();
        divan::black_box((fields[0], fields[8]))
    });
}

#[divan::bench(sample_count = 3, sample_size = 1)]
fn live_monitor_sample(bencher: divan::Bencher) {
    let mut reader = MonitorReader::new();
    bencher.bench_local(|| reader.sample());
}

#[divan::bench(sample_count = 3, sample_size = 1)]
fn live_process_tick(bencher: divan::Bencher) {
    let mut reader = MonitorReader::new();
    bencher.bench_local(|| reader.processes_tick());
}

fn main() { divan::main(); }
// Keep the imported constants tied to the benchmark executable's setup.
#[allow(dead_code)]
const _: &str = PROC_CPU;
#[allow(dead_code)]
const _: &str = PROC_NET;
