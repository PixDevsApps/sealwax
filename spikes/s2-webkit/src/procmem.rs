//! Memory of a process and its descendants from `/proc` (WebKit's web and network processes
//! run under bubblewrap, so they are grandchildren, not children).

use std::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub rss_kib: u64,
    /// Proportional set size; `None` when `smaps_rollup` is not readable.
    pub pss_kib: Option<u64>,
}

fn read_process(pid: u32) -> Option<Process> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat[open + 1..close].to_owned();
    let ppid = stat[close + 1..].split_whitespace().nth(1)?.parse().ok()?;
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kib = field_kib(&status, "VmRSS:").unwrap_or(0);
    let pss_kib = fs::read_to_string(format!("/proc/{pid}/smaps_rollup"))
        .ok()
        .and_then(|rollup| field_kib(&rollup, "Pss:"));
    Some(Process {
        pid,
        ppid,
        name,
        rss_kib,
        pss_kib,
    })
}

fn field_kib(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse().ok())
}

/// `root` and every process below it.
pub fn tree(root: u32) -> Vec<Process> {
    let all: Vec<Process> = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .filter_map(read_process)
        .collect();
    let mut found: Vec<Process> = all.iter().filter(|p| p.pid == root).cloned().collect();
    let mut index = 0;
    while index < found.len() {
        let parent = found[index].pid;
        found.extend(all.iter().filter(|p| p.ppid == parent).cloned());
        index += 1;
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_this_process_and_its_memory() {
        let processes = tree(std::process::id());
        assert_eq!(processes[0].pid, std::process::id());
        assert!(processes[0].rss_kib > 0);
        assert_eq!(field_kib("VmRSS:\t  1234 kB\n", "VmRSS:"), Some(1234));
    }
}
