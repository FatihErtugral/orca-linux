pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const REPO: &str = "FatihErtugral/orca-linux";

/// Numeric, piecewise semver comparison; tolerates a leading "v".
pub fn is_newer(remote: &str, local: &str) -> bool {
    let remote_parts = components(remote);
    let local_parts = components(local);
    let count = remote_parts.len().max(local_parts.len());
    for index in 0..count {
        let r = remote_parts.get(index).copied().unwrap_or(0);
        let l = local_parts.get(index).copied().unwrap_or(0);
        if r != l {
            return r > l;
        }
    }
    false
}

fn components(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches(['v', 'V'])
        .split('.')
        .map(|part| {
            part.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_semver_piecewise() {
        assert!(is_newer("0.2.0", "0.1.9"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.10", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
    }

    #[test]
    fn tolerates_v_prefix_and_length_differences() {
        assert!(is_newer("v0.2", "0.1.9"));
        assert!(!is_newer("v0.1", "0.1.0"));
        assert!(is_newer("0.1.0.1", "0.1"));
    }

    #[test]
    fn non_numeric_suffixes_parse_as_leading_digits() {
        assert!(is_newer("0.2-rc1", "0.1.9"));
        assert!(!is_newer("garbage", "0.0.1"));
    }
}
