use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

use crate::commands::init;
use crate::ConfigCommands;

// ---------------------------------------------------------------------------
// Config key registry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
enum ConfigType {
    Bool,
    Enum(&'static [&'static str]),
    StringArray,
}

struct ConfigKey {
    key: &'static str,
    config_type: ConfigType,
    description: &'static str,
}

static REGISTRY: &[ConfigKey] = &[
    ConfigKey {
        key: "tracking_mode",
        config_type: ConfigType::Enum(&["strict", "normal", "relaxed"]),
        description: "How aggressively issue tracking is enforced before code changes",
    },
    ConfigKey {
        key: "intervention_tracking",
        config_type: ConfigType::Bool,
        description: "Log driver interventions for autonomy improvement",
    },
    ConfigKey {
        key: "cpitd_auto_install",
        config_type: ConfigType::Bool,
        description: "Automatically install cpitd (context-provider) during init",
    },
    ConfigKey {
        key: "comment_discipline",
        config_type: ConfigType::Enum(&["encouraged", "required", "relaxed"]),
        description: "How strictly typed comments are enforced on issues",
    },
    ConfigKey {
        key: "kickoff_verification",
        config_type: ConfigType::Enum(&["local", "ci", "none"]),
        description: "Verification mode for agent kickoff tasks",
    },
    ConfigKey {
        key: "signing_enforcement",
        config_type: ConfigType::Enum(&["disabled", "audit", "enforced"]),
        description: "SSH signature verification level for coordination branch",
    },
    ConfigKey {
        key: "reminder_drift_threshold",
        config_type: ConfigType::Enum(&["0", "3", "5", "10", "15"]),
        description: "Prompts without crosslink usage before re-injecting reminder (0 = always)",
    },
    ConfigKey {
        key: "blocked_git_commands",
        config_type: ConfigType::StringArray,
        description: "Git mutation commands blocked in all tracking modes",
    },
    ConfigKey {
        key: "gated_git_commands",
        config_type: ConfigType::StringArray,
        description: "Git commands allowed only with explicit user approval",
    },
    ConfigKey {
        key: "allowed_bash_prefixes",
        config_type: ConfigType::StringArray,
        description: "Bash commands that bypass the issue-required check",
    },
];

fn find_registry_key(key: &str) -> Option<&'static ConfigKey> {
    REGISTRY.iter().find(|k| k.key == key)
}

fn type_label(ct: ConfigType) -> &'static str {
    match ct {
        ConfigType::Bool => "bool",
        ConfigType::Enum(_) => "enum",
        ConfigType::StringArray => "string[]",
    }
}

// ---------------------------------------------------------------------------
// Read / write helpers
// ---------------------------------------------------------------------------

fn read_config(crosslink_dir: &Path) -> Result<serde_json::Value> {
    let path = crosslink_dir.join("hook-config.json");
    let content =
        fs::read_to_string(&path).context("Failed to read .crosslink/hook-config.json")?;
    serde_json::from_str(&content).context("Failed to parse hook-config.json")
}

fn write_config(crosslink_dir: &Path, config: &serde_json::Value) -> Result<()> {
    let path = crosslink_dir.join("hook-config.json");
    let pretty = serde_json::to_string_pretty(config).context("Failed to serialize config")?;
    fs::write(&path, format!("{pretty}\n")).context("Failed to write hook-config.json")
}

fn read_defaults() -> Result<serde_json::Value> {
    serde_json::from_str(init::HOOK_CONFIG_JSON).context("embedded hook-config.json is invalid")
}

fn format_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Array(arr) => {
            let items: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            items.join(", ")
        }
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Subcommand dispatch
// ---------------------------------------------------------------------------

pub fn run(command: ConfigCommands, crosslink_dir: &Path) -> Result<()> {
    match command {
        ConfigCommands::Show => show(crosslink_dir),
        ConfigCommands::Get { key } => get(crosslink_dir, &key),
        ConfigCommands::Set {
            key,
            value,
            add,
            remove,
        } => set(
            crosslink_dir,
            &key,
            value.as_deref(),
            add.as_deref(),
            remove.as_deref(),
        ),
        ConfigCommands::List => list(),
        ConfigCommands::Reset { key } => reset(crosslink_dir, key.as_deref()),
        ConfigCommands::Diff => diff(crosslink_dir),
    }
}

// ---------------------------------------------------------------------------
// show — print all config with default annotations
// ---------------------------------------------------------------------------

fn show(crosslink_dir: &Path) -> Result<()> {
    let config = read_config(crosslink_dir)?;
    let defaults = read_defaults()?;

    for entry in REGISTRY {
        let current = config.get(entry.key);
        let default = defaults.get(entry.key);
        let current_str = current
            .map(format_value)
            .unwrap_or_else(|| "(unset)".into());
        let is_default = current == default;
        let annotation = if is_default {
            "(default)"
        } else {
            "(modified)"
        };

        if matches!(entry.config_type, ConfigType::StringArray) {
            println!(
                "{} {} {}:",
                entry.key,
                annotation,
                type_label(entry.config_type)
            );
            if let Some(serde_json::Value::Array(arr)) = current {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        println!("  - {s}");
                    }
                }
            }
        } else {
            println!("{} = {} {}", entry.key, current_str, annotation);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// get — print a single value
// ---------------------------------------------------------------------------

fn get(crosslink_dir: &Path, key: &str) -> Result<()> {
    if find_registry_key(key).is_none() {
        bail!("Unknown config key: \"{key}\". Run `crosslink config list` to see available keys.");
    }

    let config = read_config(crosslink_dir)?;
    match config.get(key) {
        Some(serde_json::Value::Array(arr)) => {
            for item in arr {
                if let Some(s) = item.as_str() {
                    println!("{s}");
                }
            }
        }
        Some(v) => println!("{}", format_value(v)),
        None => println!("(unset)"),
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// set — validate and write a config value
// ---------------------------------------------------------------------------

fn set(
    crosslink_dir: &Path,
    key: &str,
    value: Option<&str>,
    add: Option<&str>,
    remove: Option<&str>,
) -> Result<()> {
    let entry = find_registry_key(key).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown config key: \"{key}\". Run `crosslink config list` to see available keys."
        )
    })?;

    let mut config = read_config(crosslink_dir)?;

    match entry.config_type {
        ConfigType::Bool => {
            let val = value
                .ok_or_else(|| anyhow::anyhow!("Usage: crosslink config set {key} <true|false>"))?;
            match val {
                "true" => config[key] = serde_json::Value::Bool(true),
                "false" => config[key] = serde_json::Value::Bool(false),
                _ => {
                    bail!("Invalid value for {key}: expected \"true\" or \"false\", got \"{val}\"")
                }
            }
            write_config(crosslink_dir, &config)?;
            println!("{key} = {val}");
        }
        ConfigType::Enum(valid) => {
            let val = value.ok_or_else(|| {
                anyhow::anyhow!("Usage: crosslink config set {key} <{}>", valid.join("|"))
            })?;
            if !valid.contains(&val) {
                bail!(
                    "Invalid value for {key}: \"{val}\". Valid values: {}",
                    valid.join(", ")
                );
            }
            config[key] = serde_json::Value::String(val.to_string());
            write_config(crosslink_dir, &config)?;
            println!("{key} = {val}");
        }
        ConfigType::StringArray => {
            if let Some(item) = add {
                let arr = config[key]
                    .as_array_mut()
                    .ok_or_else(|| anyhow::anyhow!("{key} is not an array in config"))?;
                let already = arr.iter().any(|v| v.as_str() == Some(item));
                if already {
                    println!("\"{item}\" already in {key}");
                } else {
                    arr.push(serde_json::Value::String(item.to_string()));
                    write_config(crosslink_dir, &config)?;
                    println!("Added \"{item}\" to {key}");
                }
            } else if let Some(item) = remove {
                let arr = config[key]
                    .as_array_mut()
                    .ok_or_else(|| anyhow::anyhow!("{key} is not an array in config"))?;
                let before = arr.len();
                arr.retain(|v| v.as_str() != Some(item));
                if arr.len() == before {
                    println!("\"{item}\" not found in {key}");
                } else {
                    write_config(crosslink_dir, &config)?;
                    println!("Removed \"{item}\" from {key}");
                }
            } else if let Some(val) = value {
                let items: Vec<serde_json::Value> = val
                    .split(',')
                    .map(|s| serde_json::Value::String(s.trim().to_string()))
                    .collect();
                config[key] = serde_json::Value::Array(items);
                write_config(crosslink_dir, &config)?;
                println!("Set {key} to {val}");
            } else {
                bail!(
                    "Usage: crosslink config set {key} \"val1,val2,...\" or --add/--remove \"val\""
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// list — print all keys with types and descriptions
// ---------------------------------------------------------------------------

fn list() -> Result<()> {
    let defaults = read_defaults()?;

    println!("{:<28} {:<10} DESCRIPTION", "KEY", "TYPE");
    let sep = "-".repeat(78);
    println!("{sep}");

    for entry in REGISTRY {
        let default_str = defaults
            .get(entry.key)
            .map(|v| match v {
                serde_json::Value::Array(a) => format!("[{} items]", a.len()),
                other => format_value(other),
            })
            .unwrap_or_else(|| "(none)".into());

        println!(
            "{:<28} {:<10} {} (default: {})",
            entry.key,
            type_label(entry.config_type),
            entry.description,
            default_str
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// reset — restore defaults (all or single key)
// ---------------------------------------------------------------------------

fn reset(crosslink_dir: &Path, key: Option<&str>) -> Result<()> {
    let defaults = read_defaults()?;

    if let Some(key) = key {
        if find_registry_key(key).is_none() {
            bail!(
                "Unknown config key: \"{key}\". Run `crosslink config list` to see available keys."
            );
        }
        let default_val = defaults
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("No default found for {key}"))?;
        let mut config = read_config(crosslink_dir)?;
        config[key] = default_val.clone();
        write_config(crosslink_dir, &config)?;
        println!("Reset {key} to default: {}", format_value(default_val));
    } else {
        write_config(crosslink_dir, &defaults)?;
        println!("Reset all config to defaults.");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// diff — compare current vs defaults, key-by-key
// ---------------------------------------------------------------------------

fn diff(crosslink_dir: &Path) -> Result<()> {
    let config = read_config(crosslink_dir)?;
    let defaults = read_defaults()?;
    let mut any_diff = false;

    for entry in REGISTRY {
        let current = config.get(entry.key);
        let default = defaults.get(entry.key);

        if current != default {
            any_diff = true;
            let cur_str = current
                .map(format_value)
                .unwrap_or_else(|| "(unset)".into());
            let def_str = default
                .map(format_value)
                .unwrap_or_else(|| "(unset)".into());

            if matches!(entry.config_type, ConfigType::StringArray) {
                println!("{} (modified):", entry.key);
                let cur_arr: Vec<&str> = current
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                let def_arr: Vec<&str> = default
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();

                for item in &cur_arr {
                    if !def_arr.contains(item) {
                        println!("  + {item}");
                    }
                }
                for item in &def_arr {
                    if !cur_arr.contains(item) {
                        println!("  - {item}");
                    }
                }
            } else {
                println!("{}: {} (default: {})", entry.key, cur_str, def_str);
            }
        }
    }

    if !any_diff {
        println!("No differences from defaults.");
    }
    Ok(())
}
