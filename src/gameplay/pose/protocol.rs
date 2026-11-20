//! Java 26.3 packet encodings for the handful of packets the 0.2 host cannot serialize.

use crate::bindings::pumpkin::plugin::{
    common::{BlockPos, Position},
    player::{JavaPlayer, Player},
    uuid::Uuid,
};

/// Writes a positive protocol VarInt, including host-allocated runtime entity IDs.
fn varint(out: &mut Vec<u8>, mut value: u32) {
    while value >= 128 {
        out.push(value as u8 | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

/// Writes a length-prefixed UTF-8 string used in the player profile.
fn string(out: &mut Vec<u8>, value: &str) {
    varint(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

/// Encodes an ADD_PLAYER action with the owner's original signed texture property.
/// Omitting UPDATE_LISTED keeps the virtual player out of the tab list.
pub(super) fn profile(owner: &Player, id: Uuid) -> Vec<u8> {
    let mut out = vec![1, 1];
    out.extend_from_slice(&id.high.to_be_bytes());
    out.extend_from_slice(&id.low.to_be_bytes());
    string(&mut out, &owner.get_name());
    if let Some(skin) = owner.get_skin() {
        out.push(1);
        string(&mut out, "textures");
        string(&mut out, &skin.value);
        out.push(u8::from(skin.signature.is_some()));
        if let Some(signature) = skin.signature {
            string(&mut out, &signature);
        }
    } else {
        out.push(0);
    }
    out
}

/// Registers an unlisted player profile on a 26.3 connection (protocol 777).
pub(super) fn add_profile(client: &JavaPlayer, data: &[u8]) {
    client.send_raw_packet(71, data);
}

/// Packs a signed block position exactly as the Java optional sleeping-position tracker expects.
pub(super) fn packed_position(pos: BlockPos) -> [u8; 8] {
    let value = ((i64::from(pos.x) & 0x3ff_ffff) << 38)
        | ((i64::from(pos.z) & 0x3ff_ffff) << 12)
        | (i64::from(pos.y) & 0xfff);
    value.to_be_bytes()
}

/// Sends the scale affecting the avatar's visual size as an effective base without modifiers.
pub(super) fn scale(client: &JavaPlayer, entity: u32, value: f64) {
    let mut out = Vec::new();
    varint(&mut out, entity);
    out.extend_from_slice(&[1, 30]); // one attribute, minecraft:scale
    out.extend_from_slice(&value.to_be_bytes());
    out.push(0);
    client.send_raw_packet(134, &out);
}

/// Updates the body and the independent head yaw on a virtual player.
pub(super) fn rotation(client: &JavaPlayer, entity: u32, body: u8, head: u8, pitch: u8) {
    let mut out = Vec::new();
    varint(&mut out, entity);
    let prefix = out.len();
    out.extend_from_slice(&[body, pitch, 1]);
    client.send_raw_packet(57, &out);
    out.truncate(prefix);
    out.push(head);
    client.send_raw_packet(85, &out);
}

/// Teleports the avatar back after the client snaps a sleeper to its fictitious bed.
pub(super) fn correct_sleeper(client: &JavaPlayer, id: u32, pos: Position) {
    let mut out = Vec::new();
    varint(&mut out, id);
    for n in [pos.0, pos.1, pos.2] {
        out.extend_from_slice(&n.to_be_bytes());
    }
    // Zero velocity, yaw and pitch, no relative movement flags and on-ground=false.
    out.extend_from_slice(&[0; 37]);
    client.send_raw_packet(128, &out);
}

/// Mirrors the default 26.3 hand animation for six ticks on the visual avatar.
pub(super) fn swing(client: &JavaPlayer, entity: u32, off_hand: bool) {
    let mut out = Vec::new();
    varint(&mut out, entity);
    out.extend_from_slice(&[u8::from(off_hand), 1, 6]);
    client.send_raw_packet(123, &out);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Entity IDs use seven payload bits per VarInt byte, including the fifth byte at the u32 limit.
    #[test]
    fn varints_match_java_packet_boundaries() {
        for (value, expected) in [
            (0, &[0x00][..]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (16_383, &[0xff, 0x7f]),
            (16_384, &[0x80, 0x80, 0x01]),
            (u32::MAX, &[0xff, 0xff, 0xff, 0xff, 0x0f]),
        ] {
            let mut bytes = Vec::new();
            varint(&mut bytes, value);
            assert_eq!(bytes, expected, "{value}");
        }
    }

    /// The string prefix counts UTF-8 bytes and expands when their length crosses 127.
    #[test]
    fn profile_strings_prefix_the_utf8_byte_count() {
        let mut bytes = Vec::new();
        string(&mut bytes, "");
        string(&mut bytes, "猫🪑");
        assert_eq!(bytes, [0, 7, 0xe7, 0x8c, 0xab, 0xf0, 0x9f, 0xaa, 0x91]);

        let value = "é".repeat(64);
        bytes.clear();
        string(&mut bytes, &value);
        assert_eq!(&bytes[..2], &[0x80, 0x01]);
        assert_eq!(&bytes[2..], value.as_bytes());
    }

    /// Java block positions pack signed X/Z/Y fields into 26/26/12 bits in network byte order.
    #[test]
    fn sleeping_positions_encode_signed_coordinate_limits() {
        for ((x, y, z), packed) in [
            ((0, 0, 0), 0_u64),
            ((-1, -1, -1), u64::MAX),
            ((1, -1, -1), 0x0000_007f_ffff_ffff),
            ((-33_554_432, -2_048, 33_554_431), 0x8000_001f_ffff_f800),
            ((33_554_431, 2_047, -33_554_432), 0x7fff_ffe0_0000_07ff),
        ] {
            assert_eq!(
                packed_position(BlockPos { x, y, z }),
                packed.to_be_bytes(),
                "({x}, {y}, {z})"
            );
        }
    }
}
