//! `std.net`: the network, through the `Net` capability, which `main` names like `Fs`. Its
//! types are declared in Fennec, and the methods that reach the network are written in Rust.

use super::Module;

pub(crate) const MODULE: Module = Module {
    name: "net",
    names: &["Net", "Connection", "Server", "UdpSocket", "Packet", "Response", "Request", "NetError", "Certificate", "ConnectionWriter", "ConnectionReader"],
    source: include_str!("net.fen"),
    gives: &[("connect", "Connection"), ("listen", "Server"), ("udp", "UdpSocket"), ("receive", "Packet"), ("get", "Response"), ("post", "Response"), ("request", "Response")],
};
