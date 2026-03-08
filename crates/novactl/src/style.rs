//! Reusable CLI styling utilities.

use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use console::Style;
use indicatif::{ProgressBar, ProgressStyle};

/// Return a styled state string with a colored bullet.
pub fn state_styled(state: &str) -> String {
    match state {
        "running" => {
            let s = Style::new().green();
            format!("{}", s.apply_to(format!("\u{25cf} {state}")))
        }
        "created" => {
            let s = Style::new().yellow();
            format!("{}", s.apply_to(format!("\u{25cf} {state}")))
        }
        "stopped" => {
            let s = Style::new().red().dim();
            format!("{}", s.apply_to(format!("\u{25cf} {state}")))
        }
        "error" => {
            let s = Style::new().red().bold();
            format!("{}", s.apply_to(format!("\u{25cf} {state}")))
        }
        _ => {
            let s = Style::new().dim();
            format!("{}", s.apply_to(format!("\u{25cf} {state}")))
        }
    }
}

/// Print a success message with a green check mark.
pub fn success(msg: &str) {
    let s = Style::new().green();
    println!("  {} {msg}", s.apply_to("\u{2713}"));
}

/// Print an error message with a red cross.
pub fn error(msg: &str) {
    let s = Style::new().red();
    eprintln!("  {} {msg}", s.apply_to("\u{2717}"));
}

/// Print an info message with a cyan bullet.
pub fn info(msg: &str) {
    let s = Style::new().cyan();
    println!("  {} {msg}", s.apply_to("\u{25cf}"));
}

/// Print a section header.
pub fn header(title: &str) {
    let s = Style::new().bold().white();
    println!("  {}", s.apply_to(format!("\u{2550}\u{2550}\u{2550} {title} \u{2550}\u{2550}\u{2550}")));
}

/// Create a styled table with UTF-8 borders and bold cyan headers.
pub fn nova_table(headers: &[&str]) -> Table {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic);

    let header_style = Style::new().bold().cyan();
    let styled_headers: Vec<String> = headers
        .iter()
        .map(|h| format!("{}", header_style.apply_to(h)))
        .collect();

    use comfy_table::Row;
    let mut row = Row::new();
    for h in &styled_headers {
        row.add_cell(comfy_table::Cell::new(h));
    }
    table.set_header(row);
    table
}

/// Create a spinner progress bar with a message.
pub fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(&["\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}", "\u{2807}", "\u{280f}", "\u{25cf}"]),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(std::time::Duration::from_millis(80));
    pb
}

/// Format bytes as human-readable size.
pub fn format_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

/// Print a styled key-value pair.
pub fn kv(key: &str, value: &str) {
    let s = Style::new().bold();
    println!("  {:<12}{value}", format!("{}", s.apply_to(format!("{key}:"))));
}
