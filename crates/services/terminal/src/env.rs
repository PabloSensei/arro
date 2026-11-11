//! Child environments are built from an allowlist, so tokens of the parent never reach a child
//! by accident. Whoever needs more names them.

const ALLOWED: &[&str] = &[
    "APPDATA",
    "ComSpec",
    "CommonProgramFiles",
    "CommonProgramW6432",
    "HOMEDRIVE",
    "HOMEPATH",
    "LANG",
    "LOCALAPPDATA",
    "NUMBER_OF_PROCESSORS",
    "OS",
    "PATH",
    "PATHEXT",
    "PROCESSOR_ARCHITECTURE",
    "ProgramData",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "PSModulePath",
    "SystemDrive",
    "SystemRoot",
    "TEMP",
    "TMP",
    "USERDOMAIN",
    "USERNAME",
    "USERPROFILE",
    "windir",
];

pub const TERM_VARS: &[(&str, &str)] = &[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")];

/// The values of the given names that are set in this process.
pub fn inherited(names: &[&str]) -> Vec<(String, String)> {
    let value = |key: &&str| {
        std::env::var(key)
            .ok()
            .map(|value| (key.to_string(), value))
    };
    names.iter().filter_map(value).collect()
}

/// What every child gets: the allowlist of this process plus the terminal variables.
pub fn allowlisted() -> Vec<(String, String)> {
    let mut env = inherited(ALLOWED);
    let vars = TERM_VARS
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()));
    env.extend(vars);
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_of_the_parent_stay_out() {
        let names: Vec<String> = allowlisted().into_iter().map(|(key, _)| key).collect();

        assert!(names.iter().any(|key| key.eq_ignore_ascii_case("PATH")));
        assert!(names.iter().any(|key| key == "TERM"));
        for secret in ["ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GITHUB_TOKEN"] {
            assert!(!names.iter().any(|key| key == secret));
        }
    }
}
