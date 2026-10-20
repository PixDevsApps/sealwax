//! Network, sync, the op queue and secrets, on a private tokio runtime.
//! The app talks to it only through plain `Send` messages; no tokio type leaves this crate.

pub mod accounts;
pub mod api;
pub mod autoconfig;
pub mod bodies;
pub(crate) mod folders;
#[cfg(any(test, feature = "fuzz"))]
#[doc(hidden)]
pub mod fuzz;
pub mod imap;
pub mod net;
pub mod oauth;
pub mod pop3;
pub mod secrets;
pub mod service;
pub mod smtp;
pub mod updates;
pub mod verify;

/// The engine's runtime: two worker threads, so sync never competes with the GTK thread.
pub fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("sealwax-engine")
        .enable_all()
        .build()
}

#[cfg(test)]
mod tests {
    #[test]
    fn runtime_runs_a_task() {
        let runtime = super::runtime().unwrap();
        assert_eq!(runtime.block_on(async { 1 + 1 }), 2);
    }
}
