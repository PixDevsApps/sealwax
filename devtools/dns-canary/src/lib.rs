//! A DNS canary for the hostile-HTML suite (ML-065), never shipped. Preloaded into a test
//! process (`LD_PRELOAD`), it is inherited by the WebKit processes that process starts, and
//! it takes over `getaddrinfo`, which GLib's resolver uses, and so WebKit's network process
//! for DNS prefetching, preconnects and every load by name. Each name looked up is appended
//! to the file `SEALWAX_DNS_CANARY_LOG` names, one line per lookup (the process's name, a
//! tab, the name), and the lookup fails with `EAI_NONAME`, so no query leaves the machine.
//! IP literals, which need no DNS, go to the real `getaddrinfo`.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::Write;
use std::net::IpAddr;

type GetAddrInfo = unsafe extern "C" fn(
    *const c_char,
    *const c_char,
    *const libc::addrinfo,
    *mut *mut libc::addrinfo,
) -> c_int;

/// `getaddrinfo(3)`, recording and refusing every name.
///
/// # Safety
///
/// The arguments are `getaddrinfo(3)`'s: `node` and `service` null or NUL-terminated, `hints`
/// null or valid, `res` valid.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn getaddrinfo(
    node: *const c_char,
    service: *const c_char,
    hints: *const libc::addrinfo,
    res: *mut *mut libc::addrinfo,
) -> c_int {
    if !node.is_null() {
        let name = unsafe { CStr::from_ptr(node) }.to_string_lossy();
        if !numeric(&name) {
            record(&name);
            return libc::EAI_NONAME;
        }
    }
    let real: *mut c_void = unsafe { libc::dlsym(libc::RTLD_NEXT, c"getaddrinfo".as_ptr()) };
    if real.is_null() {
        return libc::EAI_FAIL;
    }
    let real = unsafe { std::mem::transmute::<*mut c_void, GetAddrInfo>(real) };
    unsafe { real(node, service, hints, res) }
}

fn numeric(name: &str) -> bool {
    let name = name.trim_start_matches('[').trim_end_matches(']');
    let name = name.split('%').next().unwrap_or(name);
    name.parse::<IpAddr>().is_ok()
}

fn record(name: &str) {
    let Some(path) = std::env::var_os("SEALWAX_DNS_CANARY_LOG") else {
        return;
    };
    let process = std::fs::read_to_string("/proc/self/comm").unwrap_or_default();
    let line = format!("{}\t{}\n", process.trim(), name.replace(['\n', '\t'], " "));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}
