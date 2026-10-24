//! Provision the Command Code route on a DeepSeek Harness ACP profile.
//!
//! `a4aa-dsh mount --model <id>[=<name>] ...` reads
//! `$DSH_HOME/.credentials.yaml`, requires the `COMMAND_CODE_API_KEY`
//! reference, and installs the `@deepseek-ai/dsh-llm-pi-ai` provider patch at
//! `$DSH_HOME/profiles/acp/cordis.patch.yml`. DSH advertises the route on the
//! next `session/new`; no secret value is read or printed.
//!
//! The route's models are host configuration: pass each one explicitly. The
//! binary never guesses a catalog and never probes a metered endpoint.

use a4aa_dsh::{mount_command_code, CommandCodeModel, MountPolicy, COMMAND_CODE_BASE_URL};
use std::path::PathBuf;

const USAGE: &str = "\
usage: a4aa-dsh mount [--home <dsh-home>] [--base-url <https-url>] \\
       [--model <id>[=<name>]]... [--replace]

  --home      DSH home directory (default: $DSH_HOME, else $HOME/.dsh)
  --base-url  OpenAI-compatible gateway (default: the canonical Command Code URL)
  --model     One model to advertise; repeat for each model
  --replace   Replace a different existing profile patch
";

fn main() {
    std::process::exit(run(std::env::args().skip(1).collect()));
}

fn run(args: Vec<String>) -> i32 {
    if args.first().map(String::as_str) != Some("mount") {
        eprint!("{USAGE}");
        return 2;
    }
    let mut home: Option<PathBuf> = None;
    let mut base_url = COMMAND_CODE_BASE_URL.to_owned();
    let mut models = Vec::new();
    let mut replace = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--home" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprint!("{USAGE}");
                    return 2;
                };
                home = Some(PathBuf::from(value));
            }
            "--base-url" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprint!("{USAGE}");
                    return 2;
                };
                base_url = value.clone();
            }
            "--model" => {
                index += 1;
                let Some(value) = args.get(index) else {
                    eprint!("{USAGE}");
                    return 2;
                };
                let (id, name) = match value.split_once('=') {
                    Some((id, name)) => (id, name),
                    None => (value.as_str(), value.as_str()),
                };
                if id.trim().is_empty() {
                    eprintln!("a4aa-dsh: --model needs a non-empty model id");
                    return 2;
                }
                models.push(CommandCodeModel {
                    id: id.to_owned(),
                    name: name.to_owned(),
                });
            }
            "--replace" => replace = true,
            other => {
                eprintln!("a4aa-dsh: unknown argument `{other}`");
                eprint!("{USAGE}");
                return 2;
            }
        }
        index += 1;
    }
    if models.is_empty() {
        eprintln!("a4aa-dsh: at least one --model is required");
        return 2;
    }
    let home = match home.or_else(default_home) {
        Some(home) => home,
        None => {
            eprintln!("a4aa-dsh: set DSH_HOME or HOME, or pass --home");
            return 2;
        }
    };
    let policy = if replace {
        MountPolicy::Replace
    } else {
        MountPolicy::CreateOnly
    };
    match mount_command_code(&home, &base_url, &models, policy) {
        Ok(mounted) => {
            let state = if mounted.changed {
                "mounted"
            } else {
                "already mounted"
            };
            println!(
                "command-code {state} at {} ({} model(s))",
                mounted.patch_path.display(),
                models.len()
            );
            0
        }
        Err(error) => {
            eprintln!("a4aa-dsh: {error}");
            1
        }
    }
}

fn default_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("DSH_HOME") {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".dsh"))
}
