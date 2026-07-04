use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq, Default)]
pub struct ParsedArguments {
    pub flags: HashMap<String, String>,
    pub rest: Vec<String>,
}

/// Parses `--flag value` pairs. Everything after a lone `--` is captured as the
/// passthrough command in `rest` (used by `wrap`).
pub fn parse(args: &[String]) -> ParsedArguments {
    let mut flags = HashMap::new();
    let mut rest = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg == "--" {
            rest = args[index + 1..].to_vec();
            break;
        }
        if let Some(key) = arg.strip_prefix("--") {
            if index + 1 < args.len() && !args[index + 1].starts_with("--") {
                flags.insert(key.to_string(), args[index + 1].clone());
                index += 2;
                continue;
            }
            flags.insert(key.to_string(), String::new());
        }
        index += 1;
    }
    ParsedArguments { flags, rest }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_flag_value_pairs() {
        let parsed = parse(&strings(&[
            "--source",
            "claude-code",
            "--status",
            "running",
        ]));
        assert_eq!(parsed.flags["source"], "claude-code");
        assert_eq!(parsed.flags["status"], "running");
        assert!(parsed.rest.is_empty());
    }

    #[test]
    fn flag_without_value_becomes_empty_string() {
        let parsed = parse(&strings(&["--check"]));
        assert_eq!(parsed.flags["check"], "");
    }

    #[test]
    fn flag_followed_by_flag_gets_empty_value() {
        let parsed = parse(&strings(&["--check", "--status", "done"]));
        assert_eq!(parsed.flags["check"], "");
        assert_eq!(parsed.flags["status"], "done");
    }

    #[test]
    fn captures_everything_after_double_dash() {
        let parsed = parse(&strings(&["--title", "T", "--", "sleep", "--", "5"]));
        assert_eq!(parsed.flags["title"], "T");
        assert_eq!(parsed.rest, strings(&["sleep", "--", "5"]));
    }

    #[test]
    fn ignores_bare_positional_arguments() {
        let parsed = parse(&strings(&["stray", "--status", "done"]));
        assert_eq!(parsed.flags["status"], "done");
        assert!(parsed.rest.is_empty());
        assert_eq!(parsed.flags.len(), 1);
    }

    #[test]
    fn empty_input_parses_to_empty() {
        assert_eq!(parse(&[]), ParsedArguments::default());
    }
}
