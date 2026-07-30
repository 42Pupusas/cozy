//! The process list panel: the right-hand card listing PID, name, a CPU-bar,
//! and memory for every process `MonitorReader::processes_tick` reports (most
//! costly first).
//!
//! Pulled out of `Painter::paint` because -- like the four telemetry cards --
//! its column layout, table header, and per-row draw calls are a fully
//! self-contained concern with its own geometry constants. Unlike the
//! telemetry charts, this panel has no incremental repaint path: process
//! identity and order can change from tick to tick (a process exiting shifts
//! every row below it upward), so there is no cheap "just redraw the strip
//! that changed" the way there is for a chart's trailing edge or a moved bar.
//! It only ever paints on a full repaint, exactly like the card labels
//! `Painter::card_text` draws.

use platform::draw::{Canvas, Rect};
use statusbar::TextRenderer;
use sysmon::ProcessInfo;

use crate::MonitorPalette;
use monitor::text_layout::TextLayout;

/// Resolved x-coordinates for the table's four columns, computed once from
/// the panel's rect so the header row and every data row draw against the
/// exact same boundaries. Splitting this out of `ProcessTable::paint` is what
/// let the header and the row loop become two separate, individually
/// reasonably-sized methods instead of one function mixing geometry setup,
/// header labels, and a per-row draw loop together.
struct Columns {
    left: i32,
    right: i32,
    name_left: i32,
    cpu_right: i32,
    mem_right: i32,
}

impl Columns {
    const fn new(rect: Rect, inset: i32) -> Self {
        let left = rect.x + inset;
        let right = rect.x + rect.w - inset;
        let pid_right = left + 48;
        Self {
            left,
            right,
            name_left: pid_right + 12,
            cpu_right: right - 76,
            mem_right: right,
        }
    }
}

/// Namespace for the process table's paint routine. Holds no state across
/// frames (see module docs for why there is no incremental path to cache
/// against), so it is a set of column-geometry constants plus one paint call
/// rather than a struct instance threaded through `Painter`.
pub struct ProcessTable;

impl ProcessTable {
    /// Draw the table header and as many process rows as fit in `rect`'s
    /// height. Always called from inside the caller's `full_repaint` branch;
    /// this only paints (returns nothing) rather than reporting damage,
    /// since a full repaint's damage is already the whole window.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        canvas: &mut Canvas,
        rect: Rect,
        processes: &[ProcessInfo],
        palette: &MonitorPalette,
        title_text: &mut TextRenderer,
        subtitle_text: &mut TextRenderer,
        text: &mut TextRenderer,
    ) {
        let inset = 14;
        let columns = Columns::new(rect, inset);
        Self::header(
            canvas,
            rect,
            &columns,
            processes.len(),
            palette,
            title_text,
            subtitle_text,
        );
        let row_height = 18;
        let first_row = rect.y + 59;
        let available_height = (rect.y + rect.h - inset - first_row).max(0);
        let row_count = usize::try_from(available_height / row_height).unwrap_or(0);
        for (row, process) in processes.iter().take(row_count).enumerate() {
            let y = first_row + i32::try_from(row).unwrap_or(0) * row_height;
            Self::row(canvas, &columns, y, row, process, palette, text);
        }
    }

    /// The title, task count, and column labels -- painted once per full
    /// repaint regardless of how many process rows follow.
    fn header(
        canvas: &mut Canvas,
        rect: Rect,
        columns: &Columns,
        process_count: usize,
        palette: &MonitorPalette,
        title_text: &mut TextRenderer,
        subtitle_text: &mut TextRenderer,
    ) {
        title_text.draw_text(
            canvas,
            "Processes",
            columns.left,
            rect.y + 14,
            palette.dim,
            palette.surface,
        );
        subtitle_text.draw_text_right(
            canvas,
            &format!("{process_count} tasks"),
            columns.right,
            rect.y + 15,
            palette.faint,
            palette.surface,
        );
        subtitle_text.draw_text(
            canvas,
            "PID",
            columns.left,
            rect.y + 40,
            palette.faint,
            palette.surface,
        );
        subtitle_text.draw_text(
            canvas,
            "Name",
            columns.name_left,
            rect.y + 40,
            palette.faint,
            palette.surface,
        );
        subtitle_text.draw_text_right(
            canvas,
            "CPU",
            columns.cpu_right,
            rect.y + 40,
            palette.faint,
            palette.surface,
        );
        subtitle_text.draw_text_right(
            canvas,
            "Memory",
            columns.mem_right,
            rect.y + 40,
            palette.faint,
            palette.surface,
        );
    }

    /// One process's PID, name, CPU bar, and memory value at row-top `y`.
    /// `row == 0` (the costliest process) is highlighted in the accent color
    /// used for the temperature card, matching the original hardcoded
    /// behavior.
    fn row(
        canvas: &mut Canvas,
        columns: &Columns,
        y: i32,
        row: usize,
        process: &ProcessInfo,
        palette: &MonitorPalette,
        text: &mut TextRenderer,
    ) {
        let name_width = (columns.cpu_right - columns.name_left - 12).max(1);
        let name = TextLayout::truncate_prefix(&process.name, text, name_width);
        text.draw_text(
            canvas,
            &process.pid.to_string(),
            columns.left,
            y,
            palette.dim,
            palette.surface,
        );
        text.draw_text(
            canvas,
            &name,
            columns.name_left,
            y,
            if row == 0 {
                palette.temperature
            } else {
                palette.foreground
            },
            palette.surface,
        );
        Self::cpu_bar(canvas, columns.cpu_right, y, row, process, palette);
        text.draw_text_right(
            canvas,
            &format!(
                "{:.1} MB",
                process
                    .memory_kb
                    .to_string()
                    .parse::<f32>()
                    .unwrap_or(f32::MAX)
                    / 1024.0
            ),
            columns.mem_right,
            y,
            palette.dim,
            palette.surface,
        );
    }

    /// The CPU column's visual bar: a background track plus a filled portion
    /// proportional to `process.cpu`, saturating at 100% for multi-threaded
    /// processes that can exceed one core.
    fn cpu_bar(
        canvas: &mut Canvas,
        bar_right: i32,
        y: i32,
        row: usize,
        process: &ProcessInfo,
        palette: &MonitorPalette,
    ) {
        let bar_left = bar_right - 58;
        let bar_y = y + 5;
        canvas.fill_rect(
            Rect::new(bar_left, bar_y, bar_right - bar_left, 6),
            palette.border,
        );
        let bar_width = bar_right - bar_left;
        let fill = (process.cpu.clamp(0.0, 100.0) / 100.0
            * bar_width.to_string().parse::<f32>().unwrap_or(0.0))
        .round()
        .to_string()
        .parse::<i32>()
        .unwrap_or(bar_width);
        if fill > 0 {
            canvas.fill_rect(
                Rect::new(bar_left, bar_y, fill, 6),
                if row == 0 {
                    palette.temperature
                } else {
                    palette.cpu
                },
            );
        }
    }
}
