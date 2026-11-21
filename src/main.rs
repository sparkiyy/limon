mod config;
mod env_transform;
mod processor;
mod transform;

use config::LimonConfig;
use processor::process_project;
use std::env;
use std::path::PathBuf;
use std::process;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug)]
enum Subcommand {
    Process,
}

#[derive(Debug)]
struct CliArgs {
    command: Option<Subcommand>,
    path: PathBuf,
    output: Option<PathBuf>,
    config: Option<PathBuf>,
    clean: bool,
    strict: bool,
    help: bool,
    version: bool,
}

fn print_help() {
    println!(
        r#"limon {} - Luau require alias expander & static env preprocessor

USAGE:
    limon <SUBCOMMAND> [PATH] [OPTIONS]

SUBCOMMANDS:
    process     Process Luau files once, inject env vars, and expand require aliases

OPTIONS:
    -o, --output <DIR>     Output directory for processed files (overrides limon.toml)
    -c, --config <FILE>    Path to custom configuration file (default: limon.toml)
        --clean            Remove orphaned files in output directory
        --strict           Fail on unresolved require aliases
    -h, --help             Print help information
    -v, --version          Print version information

EXAMPLES:
    limon process src --output dist
    limon process -o dist --clean --strict
"#,
        VERSION
    );
}

fn parse_args() -> Result<CliArgs, String> {
    let args: Vec<String> = env::args().skip(1).collect();

    let mut command = None;
    let mut positional_path = None;
    let mut output = None;
    let mut config = None;
    let mut clean = false;
    let mut strict = false;
    let mut help = false;
    let mut version = false;

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        match arg.as_str() {
            "-h" | "--help" => {
                help = true;
                i += 1;
            }
            "-v" | "--version" => {
                version = true;
                i += 1;
            }
            "--clean" => {
                clean = true;
                i += 1;
            }
            "--strict" => {
                strict = true;
                i += 1;
            }
            "-o" | "--output" => {
                if i + 1 < args.len() {
                    output = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    return Err("Option '--output' requires a directory argument.".to_string());
                }
            }
            "-c" | "--config" => {
                if i + 1 < args.len() {
                    config = Some(PathBuf::from(&args[i + 1]));
                    i += 2;
                } else {
                    return Err("Option '--config' requires a file path argument.".to_string());
                }
            }
            "process" if command.is_none() => {
                command = Some(Subcommand::Process);
                i += 1;
            }
            other if !other.starts_with('-') => {
                if command.is_none() {
                    return Err(format!("Unknown subcommand '{}'", other));
                } else if positional_path.is_none() {
                    positional_path = Some(PathBuf::from(other));
                } else {
                    return Err(format!("Unexpected argument '{}'", other));
                }
                i += 1;
            }
            unknown => {
                return Err(format!("Unrecognized option '{}'", unknown));
            }
        }
    }

    Ok(CliArgs {
        command,
        path: positional_path.unwrap_or_else(|| PathBuf::from(".")),
        output,
        config,
        clean,
        strict,
        help,
        version,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(err) => {
            eprintln!("Error: {}", err);
            eprintln!("Run 'limon --help' for usage details.");
            process::exit(1);
        }
    };

    if args.version {
        println!("limon {}", VERSION);
        return;
    }

    if args.help || args.command.is_none() {
        print_help();
        return;
    }

    let root_path = match args.path.canonicalize() {
        Ok(p) => p,
        Err(_) => args.path,
    };

    match args.command.unwrap() {
        Subcommand::Process => {
            let (mut config, _) = match LimonConfig::find_and_load(&root_path, args.config.as_deref()) {
                Ok(cfg) => cfg,
                Err(err) => {
                    eprintln!("[limon] Configuration error: {}", err);
                    process::exit(1);
                }
            };

            config.clean = config.clean || args.clean;
            config.strict = config.strict || args.strict;

            let final_output = args.output
                .or_else(|| config.output.as_ref().map(PathBuf::from));

            println!("[limon] Processing Luau project at '{}'...", root_path.display());
            if let Some(ref out) = final_output {
                println!("[limon] Target output: '{}'", out.display());
            } else {
                println!("[limon] Target output: in-place");
            }

            match process_project(&root_path, final_output.as_deref(), &config) {
                Ok(stats) => {
                    println!("[limon] Finished processing successfully.");
                    println!(
                        "  Scanned:                {} file(s)",
                        stats.files_scanned
                    );
                    println!(
                        "  Processed:              {} file(s)",
                        stats.files_processed
                    );
                    println!(
                        "  Modified:               {} file(s)",
                        stats.files_modified
                    );
                    if stats.files_removed > 0 {
                        println!(
                            "  Removed:                {} file(s)",
                            stats.files_removed
                        );
                    }
                }
                Err(err) => {
                    eprintln!("[limon] Error: {}", err);
                    process::exit(1);
                }
            }
        }
    }
}
