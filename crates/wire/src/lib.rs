//! Packet socket framing, framed marshal connections and machoNet packets.
//! Mirrors the client's blue.dll stacklessio transport and carbon/common/script/net
//! packet and address classes. Uses protocol constants, not database tables.
//! Does not own runtime configuration, session lifecycle or snappy decompression.

pub mod conn;
pub mod framing;
pub mod macho;
