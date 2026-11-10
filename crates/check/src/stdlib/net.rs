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
        ("Net", "_connect") => reads(&[Type::Str, Type::Int], fails(Type::named("Connection"))),
        ("Net", "_listen") => reads(&[Type::Int], fails(Type::named("Server"))),
        // Upgrades a plain connection, for protocols that start without TLS.
        ("Connection", "start_tls") => reads(&[Type::Str], fails(Type::Unit)),
        ("Connection", "read") => reads(&[Type::Int], fails(Type::Bytes)),
        ("Connection", "read_line") => reads(&[], fails(Type::optional(Type::Str))),
        // `bytes` or a `str`, which the call checks.
        ("Connection", "write") => reads(&[Type::Unknown], fails(Type::Unit)),
        ("Connection" | "Server" | "UdpSocket", "close") => reads(&[], Type::Unit),
        ("Server", "_accept") => reads(&[], fails(Type::named("Connection"))),
        // What `stop_on` is written with: drains when the program's asked to stop, its rounds
        // given this long.
        ("Server", "_stop_on") => reads(&[Type::named("Duration")], Type::Unit),
        // Whether its server's draining, so it should answer this message and close.
        ("Connection", "draining") => reads(&[], Type::Bool),
        ("Net", "get" | "post") => reads(&[Type::Str], fails(Type::named("Response"))),
        ("Net", "request") => reads(&[Type::Str, Type::Str], fails(Type::named("Response"))),
        ("Net", "script") => reads(&[Type::Str, Type::named("Response")], Type::Unit),
        ("Net", "requests") => reads(&[], Type::list(Type::Str)),
        ("Response", "text") => reads(&[], Type::Str),
        // The port is written `port=`, or left out for any free one.
        ("Net", "_udp") => reads(&[Type::Int], fails(Type::named("UdpSocket"))).optional(1),
        ("UdpSocket", "receive") => reads(&[], fails(Type::named("Packet"))),
        // `bytes` or a `str`, which the call checks, and an address.
        ("UdpSocket", "send_to") => reads(&[Type::Unknown, Type::Str], fails(Type::Unit)),
        _ => return None,
    })
}

