use crate::tracking::Tracker;
use crate::utils::format_tokens;
use anyhow::{Context, Result};
use colored::Colorize;

pub fn run(
    project: bool,
    _weekly: bool,
    _daily: bool,
    limit: usize,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;

    let project_scope = if project {
        let cwd = std::env::current_dir()?;
        let canonical = cwd.canonicalize().unwrap_or(cwd);
        Some(canonical.to_string_lossy().to_string())
    } else {
        None
    };

    match format {
        "json" => return export_json(&tracker, project_scope.as_deref(), limit),
        _ => {}
    }

    let entries = tracker
        .get_unoptimized_summary(project_scope.as_deref(), limit)
        .context("Failed to query unoptimized commands")?;

    if entries.is_empty() {
        println!("No unoptimized (proxy) commands recorded yet.");
        println!("Once the hook routes unmatched commands through `rtk proxy`,");
        println!("they will appear here. Run `rtk init` to install the hook.");
        return Ok(());
    }

    // Header
    println!("{}", "RTK Waste — Unoptimized Token Consumption".bold());
    println!("{}", "═".repeat(64));
    println!("Top commands running without RTK optimization:\n");

    // Table header
    println!(
        " {:<3} {:<22} {:>5}  {:>12}  {:>10}  {}",
        "#", "Command", "Count", "Total Tokens", "Avg Tokens", "Example"
    );
    println!("{}", "─".repeat(90));

    let mut total_count = 0usize;
    let mut total_tokens = 0usize;

    for (i, entry) in entries.iter().enumerate() {
        total_count += entry.count;
        total_tokens += entry.total_input_tokens;

        // Truncate example to fit
        let example = if entry.example_command.len() > 40 {
            format!("{}…", &entry.example_command[..39])
        } else {
            entry.example_command.clone()
        };

        println!(
            " {:<3} {:<22} {:>5}  {:>12}  {:>10}  {}",
            format!("{}.", i + 1),
            entry.base_command,
            entry.count,
            format_tokens(entry.total_input_tokens),
            format_tokens(entry.avg_input_tokens),
            example.dimmed(),
        );
    }

    println!("{}", "─".repeat(90));
    println!(
        "Total: {} unoptimized commands consuming ~{} tokens\n",
        total_count,
        format_tokens(total_tokens),
    );

    // Optimization coverage
    let coverage = tracker
        .get_optimization_coverage(project_scope.as_deref())
        .context("Failed to query optimization coverage")?;

    if !coverage.is_empty() {
        println!("Optimization Coverage (last {} weeks):", coverage.len());
        println!(
            "  {:<12} {:>9}  {:>11}  {:>8}",
            "Week", "Optimized", "Unoptimized", "Coverage"
        );
        for week in &coverage {
            let pct_colored = if week.coverage_pct >= 90.0 {
                format!("{:.1}%", week.coverage_pct).green()
            } else if week.coverage_pct >= 70.0 {
                format!("{:.1}%", week.coverage_pct).yellow()
            } else {
                format!("{:.1}%", week.coverage_pct).red()
            };
            println!(
                "  {:<12} {:>9}  {:>11}  {:>8}",
                week.week_start, week.filtered_count, week.proxy_count, pct_colored,
            );
        }
        println!();
    }

    println!(
        "→ Run `{}` to generate filter plans for top commands",
        "rtk reflect".bold()
    );

    Ok(())
}

fn export_json(tracker: &Tracker, project_path: Option<&str>, limit: usize) -> Result<()> {
    let entries = tracker
        .get_unoptimized_summary(project_path, limit)
        .context("Failed to query unoptimized commands")?;
    let coverage = tracker
        .get_optimization_coverage(project_path)
        .context("Failed to query optimization coverage")?;

    let output = serde_json::json!({
        "unoptimized": entries,
        "coverage": coverage,
    });

    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_waste_no_crash_on_empty_db() {
        // Verify waste doesn't crash with no proxy data
        let tracker = Tracker::new().expect("Failed to create tracker");
        let entries = tracker
            .get_unoptimized_summary(None, 15)
            .expect("query failed");
        // May be empty or have data from other tests - just verify no crash
        assert!(entries.len() <= 15);
    }
}
