mod args;
mod cli;
mod daemon;
mod paths;
mod protocol;
mod socket;
mod state_store;
mod terminal;
mod transcript;
mod tray;
mod version;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    env_logger::init();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = argv.first() else {
        print_usage();
        std::process::exit(0);
    };
    let rest = &argv[1..];

    let code = match command.as_str() {
        "event" => cli::event::run_event(rest),
        "wrap" => cli::wrap::run_wrap(rest),
        "install-hooks" => cli::hooks::run_install(),
        "uninstall-hooks" => cli::hooks::run_uninstall(),
        "tray" | "daemon" => daemon::run(rest),
        "update" => cli::update::run_update(rest),
        "--version" | "version" => {
            println!("orca v{VERSION}");
            0
        }
        "-h" | "--help" | "help" => {
            print_usage();
            0
        }
        _ => {
            eprintln!("orca: unknown command '{command}'");
            print_usage();
            2
        }
    };
    std::process::exit(code);
}

fn print_usage() {
    println!(
        "orca — agent tray for Linux (v{VERSION})

Usage:
  orca tray            Run the tray daemon (alias: daemon; --no-tray for headless)
  orca event           --status <running|waiting|done|error|idle> [--source S] [--id ID] [--title T] [--cwd DIR] [--message M]
  orca wrap            [--source S] [--title T] -- <command> [args...]
  orca install-hooks   Add Orca's Claude Code hooks to ~/.claude/settings.json
  orca uninstall-hooks Remove Orca's hooks
  orca update          Update to the latest GitHub release
  orca update --check  Only report whether an update is available

`event` also reads a Claude Code hook payload from stdin (session_id, cwd, transcript_path)."
    );
}
