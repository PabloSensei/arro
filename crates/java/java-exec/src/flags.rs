/// Without the `sun.*` pair JDK 8 and 17 turn Cyrillic in stdout and stderr into `?`
/// (measured), the other pair is what later versions read.
const OUTPUT_ENCODING: [&str; 4] = [
    "-Dsun.stdout.encoding=UTF-8",
    "-Dsun.stderr.encoding=UTF-8",
    "-Dstdout.encoding=UTF-8",
    "-Dstderr.encoding=UTF-8",
];
const FIRST_WITH_VIRTUAL_THREADS: u32 = 21;
const FIRST_WITH_LONG_OPTIONS: u32 = 9;
/// JDK 9 understands `*:port` here; JDK 8 answers `gethostbyname: unknown host`.
const FIRST_WITH_WILDCARD_ADDRESS: u32 = 9;

/// Flags that differ by the version of the target JDK, the table from the architecture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JvmFlags {
    feature: u32,
}

impl JvmFlags {
    pub fn for_jdk(feature: u32) -> Self {
        Self { feature }
    }

    pub fn output_encoding(self) -> [&'static str; 4] {
        OUTPUT_ENCODING
    }

    /// The address part of the agent. A wildcard host means every interface on JDK 9 and
    /// later; on JDK 8 it means loopback, which takes a host and a port.
    pub fn debug_address(self, host: &str, port: u16) -> String {
        if host == "*" && self.feature < FIRST_WITH_WILDCARD_ADDRESS {
            format!("127.0.0.1:{port}")
        } else {
            format!("{host}:{port}")
        }
    }

    /// The option `includevirtualthreads` is a syntax error before 21: exit 1 on 8 and 17
    /// (measured).
    pub fn debug_agent(self, host: &str, port: u16, suspend: bool) -> String {
        let suspend = if suspend { 'y' } else { 'n' };
        let address = self.debug_address(host, port);
        let threads = if self.feature >= FIRST_WITH_VIRTUAL_THREADS {
            ",includevirtualthreads=y"
        } else {
            ""
        };
        format!(
            "-agentlib:jdwp=transport=dt_socket,server=y,suspend={suspend},address={address}{threads}"
        )
    }

    /// What `javac` takes for annotation processors. On 8 the long forms are `invalid flag`
    /// (measured).
    pub fn processor_path(self, path: &str) -> Vec<String> {
        let mut args = Vec::new();
        if self.feature >= FIRST_WITH_LONG_OPTIONS {
            args.push("--processor-path".to_owned());
        } else {
            args.push("-processorpath".to_owned());
        }
        args.push(path.to_owned());
        if self.feature >= FIRST_WITH_VIRTUAL_THREADS {
            args.push("-proc:full".to_owned());
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_version_gets_all_four_encoding_flags() {
        for feature in [8, 11, 17, 21, 25] {
            assert_eq!(JvmFlags::for_jdk(feature).output_encoding().len(), 4);
        }
    }

    #[test]
    fn virtual_threads_are_asked_for_only_where_they_exist() {
        for (feature, expected) in [(8, false), (11, false), (17, false), (21, true), (25, true)] {
            let agent = JvmFlags::for_jdk(feature).debug_agent("127.0.0.1", 5005, false);

            assert!(agent.starts_with("-agentlib:jdwp="), "{agent}");
            assert_eq!(
                agent.contains("includevirtualthreads=y"),
                expected,
                "{agent}"
            );
        }
    }

    #[test]
    fn a_wildcard_address_needs_a_jdk_that_understands_it() {
        assert_eq!(
            JvmFlags::for_jdk(8).debug_address("*", 5005),
            "127.0.0.1:5005"
        );
        assert_eq!(JvmFlags::for_jdk(17).debug_address("*", 5005), "*:5005");
        assert_eq!(
            JvmFlags::for_jdk(8).debug_address("127.0.0.1", 5005),
            "127.0.0.1:5005"
        );
    }

    #[test]
    fn suspend_is_passed_through() {
        let agent = JvmFlags::for_jdk(17).debug_agent("127.0.0.1", 5005, true);

        assert!(agent.contains("suspend=y"), "{agent}");
    }

    #[test]
    fn processor_flags_follow_the_table() {
        let path = |feature| JvmFlags::for_jdk(feature).processor_path("p.jar");

        assert_eq!(path(8), ["-processorpath", "p.jar"]);
        assert_eq!(path(17), ["--processor-path", "p.jar"]);
        assert_eq!(path(21), ["--processor-path", "p.jar", "-proc:full"]);
    }
}
