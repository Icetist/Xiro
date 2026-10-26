//! Wire format of the client's POINTER and WHEEL messages; multi-byte fields are little-endian.
//! The type byte at `[0]` is not checked here: the caller dispatches on it.

/// POINTER: `[type u8][kind u8][phase u8][id u8][x u16][y u16][pressure u16][tiltX i8][tiltY i8][buttons u8][pad u8]`
pub const MSG_POINTER: u8 = 0x10;
/// WHEEL: `[type u8][pad u8][x u16][y u16][dx i16][dy i16]`
pub const MSG_WHEEL: u8 = 0x11;
/// Encoded size of a POINTER message.
pub const POINTER_BYTES: usize = 14;
/// Encoded size of a WHEEL message.
pub const WHEEL_BYTES: usize = 10;

/// POINTER without the trailing pad byte is still valid (older clients).
const POINTER_MIN_BYTES: usize = POINTER_BYTES - 1;
const U16_MAX: f64 = u16::MAX as f64;
/// Physical pen tilt never exceeds a right angle; anything beyond is a corrupt message.
const TILT_LIMIT: i32 = 90;

/// What produced a pointer event on the iPad.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Touch,
    Pen,
    Mouse,
}

impl PointerKind {
    fn from_wire(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Touch),
            1 => Some(Self::Pen),
            2 => Some(Self::Mouse),
            _ => None,
        }
    }
}

/// Lifecycle stage of a pointer event. `Hover` is a pen in range but not touching.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
    Cancel,
    Hover,
}

impl Phase {
    fn from_wire(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Down),
            1 => Some(Self::Move),
            2 => Some(Self::Up),
            3 => Some(Self::Cancel),
            4 => Some(Self::Hover),
            _ => None,
        }
    }

    /// The contact ended, normally or not.
    pub fn is_end(self) -> bool {
        matches!(self, Self::Up | Self::Cancel)
    }
}

/// One pointer sample. `x`, `y` and `pressure` are normalised to `[0, 1]`; tilt is in degrees.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pointer {
    pub kind: PointerKind,
    pub phase: Phase,
    pub id: u8,
    pub x: f64,
    pub y: f64,
    pub pressure: f64,
    pub tilt_x: i32,
    pub tilt_y: i32,
    /// `PointerEvent.buttons` bitmask: 1 left, 2 right, 4 middle.
    pub buttons: u8,
}

/// One scroll step at a normalised position. `dx`/`dy` are in WHEEL_DELTA (120) units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wheel {
    pub x: f64,
    pub y: f64,
    pub dx: i32,
    pub dy: i32,
}

fn u16_at(msg: &[u8], at: usize) -> f64 {
    f64::from(u16::from_le_bytes([msg[at], msg[at + 1]]))
}

fn i16_at(msg: &[u8], at: usize) -> i32 {
    i32::from(i16::from_le_bytes([msg[at], msg[at + 1]]))
}

fn clamp_tilt(v: i8) -> i32 {
    i32::from(v).clamp(-TILT_LIMIT, TILT_LIMIT)
}

impl Pointer {
    /// Returns `None` for truncated messages or an unknown kind/phase. `msg` includes the type byte.
    pub fn decode(msg: &[u8]) -> Option<Pointer> {
        if msg.len() < POINTER_MIN_BYTES {
            return None;
        }
        Some(Pointer {
            kind: PointerKind::from_wire(msg[1])?,
            phase: Phase::from_wire(msg[2])?,
            id: msg[3],
            x: u16_at(msg, 4) / U16_MAX,
            y: u16_at(msg, 6) / U16_MAX,
            pressure: u16_at(msg, 8) / U16_MAX,
            tilt_x: clamp_tilt(msg[10] as i8),
            tilt_y: clamp_tilt(msg[11] as i8),
            buttons: msg[12],
        })
    }
}

impl Wheel {
    /// Returns `None` for truncated messages. `msg` includes the type byte.
    pub fn decode(msg: &[u8]) -> Option<Wheel> {
        if msg.len() < WHEEL_BYTES {
            return None;
        }
        Some(Wheel { x: u16_at(msg, 2) / U16_MAX, y: u16_at(msg, 4) / U16_MAX, dx: i16_at(msg, 6), dy: i16_at(msg, 8) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn norm(v: f64) -> [u8; 2] {
        ((v.clamp(0.0, 1.0) * U16_MAX).round() as u16).to_le_bytes()
    }

    /// Same encoding as the client's `encodePointer`.
    #[allow(clippy::too_many_arguments)]
    fn encode_pointer(kind: u8, phase: u8, id: u8, x: f64, y: f64, pressure: f64, tilt: (i32, i32), buttons: u8) -> Vec<u8> {
        let clamp_i8 = |v: i32| v.clamp(-TILT_LIMIT, TILT_LIMIT) as i8 as u8;
        let mut v = vec![MSG_POINTER, kind, phase, id];
        v.extend(norm(x));
        v.extend(norm(y));
        v.extend(norm(pressure));
        v.extend([clamp_i8(tilt.0), clamp_i8(tilt.1), buttons, 0]);
        v
    }

    fn encode_wheel(x: f64, y: f64, dx: i16, dy: i16) -> Vec<u8> {
        let mut v = vec![MSG_WHEEL, 0];
        v.extend(norm(x));
        v.extend(norm(y));
        v.extend(dx.to_le_bytes());
        v.extend(dy.to_le_bytes());
        v
    }

    #[test]
    fn pointer_round_trips_within_u16_precision() {
        let msg = encode_pointer(1, 1, 3, 0.25, 0.75, 0.5, (-30, 45), 1);
        assert_eq!(msg.len(), POINTER_BYTES);
        let p = Pointer::decode(&msg).expect("valid pointer");
        assert_eq!(p.kind, PointerKind::Pen);
        assert_eq!(p.phase, Phase::Move);
        assert_eq!(p.id, 3);
        assert!((p.x - 0.25).abs() < 1e-4);
        assert!((p.y - 0.75).abs() < 1e-4);
        assert!((p.pressure - 0.5).abs() < 1e-4);
        assert_eq!((p.tilt_x, p.tilt_y), (-30, 45));
        assert_eq!(p.buttons, 1);
    }

    #[test]
    fn pointer_coordinates_and_tilt_are_clamped() {
        let p = Pointer::decode(&encode_pointer(0, 0, 0, -1.0, 2.0, 0.0, (500, -500), 0)).expect("valid");
        assert_eq!((p.x, p.y), (0.0, 1.0));
        assert_eq!((p.tilt_x, p.tilt_y), (90, -90));
    }

    #[test]
    fn every_wire_kind_and_phase_decodes() {
        let kinds = [PointerKind::Touch, PointerKind::Pen, PointerKind::Mouse];
        let phases = [Phase::Down, Phase::Move, Phase::Up, Phase::Cancel, Phase::Hover];
        for (k, kind) in kinds.iter().enumerate() {
            for (ph, phase) in phases.iter().enumerate() {
                let p = Pointer::decode(&encode_pointer(k as u8, ph as u8, 0, 0.0, 0.0, 0.0, (0, 0), 0)).expect("valid");
                assert_eq!((p.kind, p.phase), (*kind, *phase));
            }
        }
    }

    #[test]
    fn pointer_one_byte_short_is_accepted_like_the_js() {
        let msg = encode_pointer(2, 0, 0, 0.5, 0.5, 0.0, (0, 0), 4);
        assert_eq!(Pointer::decode(&msg[..POINTER_BYTES - 1]).map(|p| p.buttons), Some(4));
        assert_eq!(Pointer::decode(&msg[..POINTER_BYTES - 2]), None);
    }

    #[test]
    fn wheel_round_trips() {
        let w = Wheel::decode(&encode_wheel(0.5, 0.5, -120, 240)).expect("valid wheel");
        assert_eq!((w.dx, w.dy), (-120, 240));
        assert!((w.x - 0.5).abs() < 1e-4);
    }

    #[test]
    fn truncated_messages_decode_to_none() {
        assert_eq!(Pointer::decode(&[0; 4]), None);
        assert_eq!(Wheel::decode(&[0; 4]), None);
        assert_eq!(Wheel::decode(&encode_wheel(0.0, 0.0, 1, 1)[..WHEEL_BYTES - 1]), None);
    }

    #[test]
    fn out_of_range_kind_phase_and_tilt_are_rejected_or_clamped() {
        let mut msg = encode_pointer(0, 0, 0, 0.0, 0.0, 0.0, (0, 0), 0);
        msg[1] = 9;
        assert_eq!(Pointer::decode(&msg), None);
        msg[1] = 0;
        msg[2] = 42;
        assert_eq!(Pointer::decode(&msg), None);
        msg[2] = 0;
        msg[10] = (-128i8) as u8;
        assert_eq!(Pointer::decode(&msg).map(|p| p.tilt_x), Some(-90));
    }

    #[test]
    fn phase_end_covers_up_and_cancel() {
        assert!(Phase::Up.is_end() && Phase::Cancel.is_end());
        assert!(!Phase::Down.is_end() && !Phase::Move.is_end() && !Phase::Hover.is_end());
    }
}
