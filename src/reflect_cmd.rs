use crate::discover::provider::{ClaudeProvider, SessionProvider};
use crate::tracking::{extract_base_command, Tracker};
use crate::utils::format_tokens;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// Merged data for a single command to reflect on.
struct ReflectEntry {
    base_command: String,
    /// From tracking DB
    invocation_count: usize,
    total_tokens: usize,
    avg_tokens: usize,
    /// From JSONL sessions
    output_samples: Vec<String>,
    intent_samples: Vec<String>,
    argument_patterns: Vec<(String, usize)>, // (pattern, count)
}

pub fn run(
    project: Option<&str>,
    all: bool,
    since_days: u64,
    limit: usize,
    format: &str,
    _verbose: u8,
) -> Result<()> {
    let tracker = Tracker::new().context("Failed to initialize tracking database")?;

    let project_scope = if all {
        None
    } else {
        let cwd = std::env::current_dir()?;
        let canonical = cwd.canonicalize().unwrap_or(cwd);
        Some(canonical.to_string_lossy().to_string())
    };

    // 1. Get top unoptimized commands from tracking DB
    let unopt = tracker
        .get_unoptimized_summary(project_scope.as_deref(), limit)
        .context("Failed to query unoptimized commands")?;

    if unopt.is_empty() {
        println!("No unoptimized commands found in tracking database.");
        println!("Run some commands through the proxy first (install hook with `rtk init`).");
        return Ok(());
    }

    // 2. Try to get JSONL session data for output samples and intent
    let session_data = collect_session_data(project, all, since_days, &unopt);

    // 3. Build merged entries
    let entries: Vec<ReflectEntry> = unopt
        .iter()
        .map(|u| {
            let sd = session_data.get(&u.base_command);
            ReflectEntry {
                base_command: u.base_command.clone(),
                invocation_count: u.count,
                total_tokens: u.total_input_tokens,
                avg_tokens: u.avg_input_tokens,
                output_samples: sd.map(|s| s.output_samples.clone()).unwrap_or_default(),
                intent_samples: sd.map(|s| s.intent_samples.clone()).unwrap_or_default(),
                argument_patterns: sd.map(|s| s.argument_patterns.clone()).unwrap_or_default(),
            }
        })
        .collect();

    match format {
        "json" => print_json(&entries)?,
        _ => print_text(&entries)?,
    }

    Ok(())
}

/// Session data collected from JSONL files for a base command.
struct SessionCommandData {
    output_samples: Vec<String>,
    intent_samples: Vec<String>,
    argument_patterns: Vec<(String, usize)>,
}

/// Collect JSONL session data matching the base commands we care about.
fn collect_session_data(
    project: Option<&str>,
    all: bool,
    since_days: u64,
    unopt: &[crate::tracking::UnoptimizedEntry],
) -> HashMap<String, SessionCommandData> {
    let mut result = HashMap::new();
    let provider = ClaudeProvider;

    // Determine project filter for discover
    let project_filter = if all {
        None
    } else if let Some(p) = project {
        Some(p.to_string())
    } else {
        std::env::current_dir()
            .ok()
            .map(|cwd| ClaudeProvider::encode_project_path(&cwd.to_string_lossy()))
    };

    let sessions = match provider.discover_sessions(project_filter.as_deref(), Some(since_days)) {
        Ok(s) => s,
        Err(_) => return result,
    };

    // Build a set of base commands we're looking for
    let base_commands: Vec<&str> = unopt.iter().map(|u| u.base_command.as_str()).collect();

    // Per-command accumulators
    let mut output_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut intent_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut arg_map: HashMap<String, HashMap<String, usize>> = HashMap::new();

    for session in sessions.iter().take(100) {
        // Cap session scanning
        let commands = match provider.extract_commands(session) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for cmd in &commands {
            let base = extract_base_command(&cmd.command);
            if !base_commands.contains(&base.as_str()) {
                continue;
            }

            // Collect output sample (first 2000 chars, max 3 samples per command)
            if let Some(ref output) = cmd.output_content {
                let samples = output_map.entry(base.clone()).or_default();
                if samples.len() < 3 {
                    samples.push(output.chars().take(2000).collect());
                }
            }

            // Collect intent
            if let Some(ref ctx) = cmd.assistant_context {
                let intents = intent_map.entry(base.clone()).or_default();
                if intents.len() < 5 {
                    intents.push(ctx.clone());
                }
            }

            // Collect argument patterns
            let arg_counts = arg_map.entry(base.clone()).or_default();
            *arg_counts.entry(cmd.command.clone()).or_insert(0) += 1;
        }
    }

    // Build result
    for base in &base_commands {
        let output_samples = output_map.remove(*base).unwrap_or_default();
        let intent_samples = intent_map.remove(*base).unwrap_or_default();
        let mut argument_patterns: Vec<(String, usize)> = arg_map
            .remove(*base)
            .unwrap_or_default()
            .into_iter()
            .collect();
        argument_patterns.sort_by(|a, b| b.1.cmp(&a.1));
        argument_patterns.truncate(5);

        result.insert(
            base.to_string(),
            SessionCommandData {
                output_samples,
                intent_samples,
                argument_patterns,
            },
        );
    }

    result
}

fn print_text(entries: &[ReflectEntry]) -> Result<()> {
    println!("# RTK Filter Implementation Request\n");
    println!("## Project Context");
    println!("RTK is a CLI proxy that reduces LLM token consumption by filtering command outputs.");
    println!("Architecture: Command modules in src/*_cmd.rs, Clap routing in main.rs, tracking in tracking.rs.");
    println!(
        "Filter pattern: raw input → regex/parsing → compact output, fallback to raw on error.\n"
    );

    for (i, entry) in entries.iter().enumerate() {
        println!(
            "## Command {}: {} ({} invocations, ~{} tokens total)\n",
            i + 1,
            entry.base_command,
            entry.invocation_count,
            format_tokens(entry.total_tokens),
        );

        // Usage patterns
        if !entry.argument_patterns.is_empty() {
            println!("### Usage Patterns");
            println!(
                "Most common argument combinations (from {} tracked invocations):",
                entry.invocation_count
            );
            for (pattern, count) in &entry.argument_patterns {
                println!("- `{}` ({}x)", pattern, count);
            }
            println!();
        }

        // Output sample
        if !entry.output_samples.is_empty() {
            println!("### Output Sample");
            println!("```");
            // Show first sample, truncated
            let sample = &entry.output_samples[0];
            if sample.len() > 2000 {
                println!("{}...", &sample[..2000]);
            } else {
                println!("{}", sample);
            }
            println!("```\n");
        }

        // LLM intent
        if !entry.intent_samples.is_empty() {
            println!("### LLM Intent When Running");
            for intent in &entry.intent_samples {
                // Extract first sentence
                let first_sentence = intent.split('.').next().unwrap_or(intent).trim();
                if !first_sentence.is_empty() {
                    println!("- \"{}\"", first_sentence);
                }
            }
            println!();
        }

        // Estimated savings
        let estimated_savings_pct = 75.0; // conservative estimate based on RTK averages
        let estimated_saved = (entry.total_tokens as f64 * estimated_savings_pct / 100.0) as usize;
        println!("### Estimated Savings Potential");
        println!(
            "Raw output: avg {} tokens per invocation",
            format_tokens(entry.avg_tokens)
        );
        println!(
            "Conservative estimate ({:.0}% savings): ~{} tokens saved over tracking period",
            estimated_savings_pct,
            format_tokens(estimated_saved)
        );
        println!();

        // Suggested implementation
        let module_name = entry
            .base_command
            .split_whitespace()
            .next()
            .unwrap_or(&entry.base_command);
        println!("### Suggested Implementation");
        println!("- Module: `src/{}_cmd.rs`", module_name);
        println!(
            "- Pattern: Parse {} output, extract only essential information",
            entry.base_command
        );
        println!("- Follow: `src/git.rs` (diff filtering) as reference architecture");
        println!();

        if i < entries.len() - 1 {
            println!("---\n");
        }
    }

    Ok(())
}

fn print_json(entries: &[ReflectEntry]) -> Result<()> {
    let json_entries: Vec<serde_json::Value> = entries
        .iter()
        .map(|e| {
            serde_json::json!({
                "base_command": e.base_command,
                "invocation_count": e.invocation_count,
                "total_tokens": e.total_tokens,
                "avg_tokens": e.avg_tokens,
                "output_samples": e.output_samples,
                "intent_samples": e.intent_samples,
                "argument_patterns": e.argument_patterns.iter()
                    .map(|(cmd, count)| serde_json::json!({"command": cmd, "count": count}))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    println!("{}", serde_json::to_string_pretty(&json_entries)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reflect_entry_rendering() {
        let entry = ReflectEntry {
            base_command: "terraform plan".to_string(),
            invocation_count: 10,
            total_tokens: 50000,
            avg_tokens: 5000,
            output_samples: vec!["Terraform will perform the following actions:".to_string()],
            intent_samples: vec![
                "Let me check what infrastructure changes will be applied".to_string()
            ],
            argument_patterns: vec![
                ("terraform plan -var-file=prod.tfvars".to_string(), 5),
                ("terraform plan".to_string(), 5),
            ],
        };

        // Just verify print_text doesn't crash
        let entries = vec![entry];
        print_text(&entries).expect("print_text should not fail");
    }

    #[test]
    fn test_reflect_empty() {
        let entries: Vec<ReflectEntry> = vec![];
        print_text(&entries).expect("print_text with empty entries should not fail");
    }
}
