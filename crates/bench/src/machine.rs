//! Which CPU a run was on. CI's runners of one kind aren't all the same machine, and a time from
//! one doesn't compare with a time from another, so each run says, and its line in the history
//! keeps it.

use std::process::Command;

/// The name of this machine's CPU, or `unknown` where it doesn't say.
pub(crate) fn cpu() -> String {
    let named = if cfg!(target_os = "macos") {
        Command::new("sysctl")
            .args(["-n", "machdep.cpu.brand_string"])
            .output()
            .ok()
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|name| name.trim().to_owned())
            .filter(|name| !name.is_empty())
    } else {
        std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|text| from_cpuinfo(&text))
    };
    named.unwrap_or_else(|| "unknown".to_owned())
}

/// How many threads this machine can run at once, or 0 where it doesn't say.
pub(crate) fn cores() -> usize {
    std::thread::available_parallelism().map_or(0, std::num::NonZero::get)
}

/// The CPU `/proc/cpuinfo` describes: its model name, or on Arm, which has none, the numbers that
/// stand for it.
fn from_cpuinfo(text: &str) -> Option<String> {
    let field = |wanted: &str| {
        text.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            (name.trim() == wanted && !value.trim().is_empty()).then(|| value.trim().to_owned())
        })
    };
    field("model name").or_else(|| {
        let (implementer, part) = (field("CPU implementer")?, field("CPU part")?);
        Some(format!("implementer {implementer} part {part}"))
    })
}

#[cfg(test)]
mod tests {
    use super::from_cpuinfo;

    #[test]
    fn cpuinfo_names_the_cpu_on_x86_and_on_arm() {
        let x86 = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD EPYC 7763 64-Core Processor\n\nprocessor\t: 1\nmodel name\t: AMD EPYC 7763 64-Core Processor\n";
        assert_eq!(from_cpuinfo(x86).as_deref(), Some("AMD EPYC 7763 64-Core Processor"));
        let arm = "processor\t: 0\nBogoMIPS\t: 50.00\nCPU implementer\t: 0x41\nCPU architecture: 8\nCPU part\t: 0xd0c\n";
        assert_eq!(from_cpuinfo(arm).as_deref(), Some("implementer 0x41 part 0xd0c"));
        assert_eq!(from_cpuinfo("processor\t: 0\nmodel name\t: \n"), None);
    }
}
