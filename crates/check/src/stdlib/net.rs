//! `std.net`: the methods of the `Net` capability and of the connections and servers it makes,
//! each of which can fail with a `NetError`. The ones that wait take `timeout=`, a `Duration`.

use super::{Module, no_checks, no_functions};
use crate::builtins::{Method, reads};
use crate::types::Type;

pub(crate) const MODULE: Module =
    Module { functions: &[], result: no_functions, method, construct: no_checks };

fn method(type_name: &str, name: &str) -> Option<Method> {
    let fails = |ty: Type| Type::result(ty, Type::named("NetError"));
    Some(match (type_name, name) {
        ("Net", "connect") => reads(&[Type::Str, Type::Int], fails(Type::named("Connection"))),
        ("Net", "listen") => reads(&[Type::Int], fails(Type::named("Server"))),
        ("Connection", "read") => reads(&[Type::Int], fails(Type::Bytes)),
        ("Connection", "read_line") => reads(&[], fails(Type::optional(Type::Str))),
        // `bytes` or a `str`, which the call checks.
        ("Connection", "write") => reads(&[Type::Unknown], fails(Type::Unit)),
        ("Connection" | "Server" | "UdpSocket", "close") => reads(&[], Type::Unit),
        ("Server", "accept") => reads(&[], fails(Type::named("Connection"))),
        // The port is written `port=`, or left out for any free one.
        ("Net", "udp") => reads(&[Type::Int], fails(Type::named("UdpSocket"))).optional(1),
        ("UdpSocket", "receive") => reads(&[], fails(Type::named("Packet"))),
        // `bytes` or a `str`, which the call checks, and an address.
        ("UdpSocket", "send_to") => reads(&[Type::Unknown, Type::Str], fails(Type::Unit)),
        _ => return None,
    })
}

