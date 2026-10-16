//! Java Edition 26.3 network protocol (protocol 777): framing, VarInt, packets.
//!
//! Sans-IO: this crate turns bytes into packets and back; sockets live in
//! `mc-server`. Compression and encryption are not implemented yet.

pub mod codec;
pub mod frame;
pub mod ids;
pub mod packets;
pub mod text;

pub use codec::{DecodeError, Position, Reader, Write};
pub use ids::{Direction, PacketIds, State};
pub use packets::{Encode, Packet};

/// Protocol version whose packet layouts this crate implements.
pub const PROTOCOL_VERSION: i32 = 777;

/// Packet name not present in the id table (wrong data version).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownPacket(pub &'static str);

impl std::fmt::Display for UnknownPacket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "packet {} is not in the packet id table", self.0)
    }
}

impl std::error::Error for UnknownPacket {}

impl PacketIds {
    /// Encodes `packet` as one frame appended to `out`; `compression` is the
    /// threshold once Set Compression has been sent.
    pub fn write<P: Encode>(
        &self,
        packet: &P,
        out: &mut Vec<u8>,
        compression: Option<usize>,
    ) -> Result<(), UnknownPacket> {
        let id = self
            .id(P::STATE, Direction::Clientbound, P::NAME)
            .ok_or(UnknownPacket(P::NAME))?;
        let mut body = Vec::new();
        packet.encode(&mut body);
        frame::write_frame(out, id, &body, compression);
        Ok(())
    }
}
