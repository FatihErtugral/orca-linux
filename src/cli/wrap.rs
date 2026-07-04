use super::event::deliver;
use super::event_builder::{basename, EventBuilder};
use crate::args;
use std::os::unix::process::ExitStatusExt;
use std::process::Command;

pub fn run_wrap(argv: &[String]) -> i32 {
    let parsed = args::parse(argv);
    let Some(executable) = parsed.rest.first() else {
        eprintln!("usage: orca wrap [--source S] [--title T] -- <command> [args...]");
        return 2;
    };

    let builder = EventBuilder::system();
    let name = basename(executable);
    let id = format!("wrap:{}:{}", name, std::process::id());
    let source = parsed.flags.get("source").cloned().unwrap_or_else(|| "custom".into());
    let title = parsed.flags.get("title").cloned().unwrap_or_else(|| name.clone());
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "/".into());

    let emit = |status: &str, message: Option<String>| {
        deliver(&builder.wrap_event(&id, &source, &title, &cwd, status, message));
    };

    emit("running", None);
    let child = Command::new(executable).args(&parsed.rest[1..]).spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => {
            emit("error", Some(format!("failed to start: {error}")));
            return 127;
        }
    };
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            emit("error", Some(format!("wait failed: {error}")));
            return 127;
        }
    };

    // A signal death maps to the shell convention 128+signal.
    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));
    if code == 0 {
        emit("done", None);
    } else {
        emit("error", Some(format!("exit code {code}")));
    }
    code
}
