//! Who made a connection to a local test server. Tests that count what reaches a loopback port
//! (a canary for remote loads, a fake server counting attempts) leave out connections from
//! programs outside this process's tree, such as a desktop app that probes every new port on
//! 127.0.0.1. Read from Linux's `/proc`; anything it can't tell counts as this tree's.

use std::fs;
use std::net::SocketAddr;

/// Whether the connection from `peer` to `local`, both on this machine, was made by a process
/// that is neither this one nor started by it. `false` when that can't be told: the socket is
/// gone, or no process that holds it can be read.
pub fn from_outside(peer: SocketAddr, local: SocketAddr) -> bool {
    let Some(inode) = socket_inode(peer, local) else {
        return false;
    };
    let owners = owners(inode);
    let me = std::process::id();
    !owners.is_empty() && owners.iter().all(|&pid| !descends_from(pid, me))
}

/// `/proc/net/tcp`'s form of an address: the IPv4 address as the kernel's in-memory word, then
/// the port, both in hex.
fn proc_v4(address: SocketAddr) -> Option<String> {
    let SocketAddr::V4(address) = address else {
        return None;
    };
    Some(format!(
        "{:08X}:{:04X}",
        u32::from_ne_bytes(address.ip().octets()),
        address.port()
    ))
}

/// `/proc/net/tcp6`'s form, for IPv6 and IPv4-mapped addresses.
fn proc_v6(address: SocketAddr) -> String {
    let ip = match address {
        SocketAddr::V4(v4) => v4.ip().to_ipv6_mapped(),
        SocketAddr::V6(v6) => *v6.ip(),
    };
    let words: String = ip
        .octets()
        .chunks(4)
        .map(|word| {
            format!(
                "{:08X}",
                u32::from_ne_bytes([word[0], word[1], word[2], word[3]])
            )
        })
        .collect();
    format!("{words}:{:04X}", address.port())
}

/// The inode of the socket whose own end is `peer` and whose other end is `local`.
fn socket_inode(peer: SocketAddr, local: SocketAddr) -> Option<u64> {
    let tables = [
        ("/proc/net/tcp", proc_v4(peer), proc_v4(local)),
        ("/proc/net/tcp6", Some(proc_v6(peer)), Some(proc_v6(local))),
    ];
    for (path, own, other) in tables {
        let (Some(own), Some(other)) = (own, other) else {
            continue;
        };
        let Ok(table) = fs::read_to_string(path) else {
            continue;
        };
        for line in table.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() > 9 && fields[1] == own && fields[2] == other {
                let inode = fields[9].parse().ok()?;
                return (inode != 0).then_some(inode);
            }
        }
    }
    None
}

/// The processes that hold the socket `inode`, among those whose descriptors can be read.
fn owners(inode: u64) -> Vec<u32> {
    let target = format!("socket:[{inode}]");
    let Ok(processes) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    processes
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            fs::read_dir(format!("/proc/{pid}/fd")).is_ok_and(|fds| {
                fds.flatten().any(|fd| {
                    fs::read_link(fd.path()).is_ok_and(|link| link.as_os_str() == target.as_str())
                })
            })
        })
        .collect()
}

/// Whether `pid` is `ancestor` or was started by it, through its parents.
fn descends_from(mut pid: u32, ancestor: u32) -> bool {
    for _ in 0..4096 {
        if pid == ancestor {
            return true;
        }
        match parent(pid) {
            Some(next) if next != 0 && next != pid => pid = next,
            _ => return false,
        }
    }
    false
}

fn parent(pid: u32) -> Option<u32> {
    let status = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))?
        .trim()
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::process::Command;
    use std::time::{Duration, Instant};

    #[test]
    fn addresses_in_procs_form() {
        let address: SocketAddr = "127.0.0.1:993".parse().unwrap();
        let expected = if cfg!(target_endian = "little") {
            "0100007F:03E1"
        } else {
            "7F000001:03E1"
        };
        assert_eq!(proc_v4(address).as_deref(), Some(expected));
        assert!(proc_v6(address).ends_with(":03E1"));
        assert_eq!(proc_v6(address).len(), 32 + 5);
    }

    #[test]
    fn this_process_is_not_outside() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let local = listener.local_addr().unwrap();
        let _client = TcpStream::connect(local).unwrap();
        let (_server, peer) = listener.accept().unwrap();
        assert!(!from_outside(peer, local));
    }

    #[test]
    fn a_child_is_not_outside() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let local = listener.local_addr().unwrap();
        let Ok(mut child) = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "exec 3<>/dev/tcp/127.0.0.1/{}; sleep 5",
                local.port()
            ))
            .spawn()
        else {
            return;
        };
        let (_server, peer) = listener.accept().unwrap();
        let outside = from_outside(peer, local);
        let _ = child.kill();
        let _ = child.wait();
        assert!(!outside);
    }

    /// A process whose parent has gone is adopted outside this tree, as another program's is.
    #[test]
    fn an_orphan_is_outside() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let local = listener.local_addr().unwrap();
        let Ok(status) = Command::new("bash")
            .arg("-c")
            .arg(format!(
                "(exec 3<>/dev/tcp/127.0.0.1/{}; sleep 3) >/dev/null 2>&1 &",
                local.port()
            ))
            .status()
        else {
            return;
        };
        assert!(status.success());
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        let (_server, peer) = loop {
            match listener.accept() {
                Ok(accepted) => break accepted,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => panic!("{error}"),
            }
        };
        assert!(from_outside(peer, local));
    }
}
