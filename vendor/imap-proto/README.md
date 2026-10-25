# imap-proto

[![Build status](https://github.com/djc/tokio-imap/workflows/CI/badge.svg)](https://github.com/djc/tokio-imap/actions?query=workflow%3ACI)
[![crates.io, downloads](https://img.shields.io/crates/d/imap-proto.svg)](https://crates.io/crates/imap-proto)
[![crates.io, latest release](https://img.shields.io/crates/v/imap-proto.svg)](https://crates.io/crates/imap-proto)
[![API docs, latest release](https://docs.rs/imap-proto/badge.svg)](http://docs.rs/imap-proto)
[![Chat](https://img.shields.io/discord/976380008299917365?logo=discord)](https://discord.gg/ujJ4pGzkGU)
[![MIT license](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE-MIT)
[![Apache License 2.0](https://img.shields.io/badge/license-ALv2-blue.svg)](./LICENSE-APACHE)

All feedback welcome. Feel free to file bugs, requests for documentation and
any other feedback to the [issue tracker][issues].

imap-proto was created and is maintained by Dirkjan Ochtman. If you depend on this
projects, please support the project via [GitHub Sponsors] or contact me for support.

[issues]: https://github.com/djc/tokio-imap/issues
[GitHub Sponsors]: https://github.com/sponsors/djc

## Features

imap-proto is a low-level IMAP protocol support crate, using the type system to
provide a safe API. The code tries to closely follow the [IMAP4rev1 RFC][rfc3501], plus
several extensions.

Protocol support is implemented in three parts:

* Types that attempt to closely reflect specification requirements
* A parser implementation to help consume protocol messages
* Builder types to help produce protocol messages

[rfc3501]: https://tools.ietf.org/html/rfc3501
