//! H.264 bitstream handling: encoders emit Annex B (start codes), WebCodecs on the
//! iPad wants AVCC (length-prefixed NAL units) plus an avcC decoder configuration.

const NAL_IDR: u8 = 5;
const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_AUD: u8 = 9;
/// Profiles whose avcC carries the chroma / bit-depth extension (ISO/IEC 14496-15 5.3.3.1).
const HIGH_PROFILES: [u8; 4] = [100, 110, 122, 144];

/// One encoded picture, ready for the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    pub is_key: bool,
    /// Length-prefixed NAL units (4-byte big-endian lengths), parameter sets removed.
    pub avcc: Vec<u8>,
    /// Parameter sets carried in this unit (encoders repeat them before key frames).
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
}

/// Splits an Annex B stream into NAL units (without start codes).
pub fn nal_units(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(n, &start)| match starts.get(n + 1) {
            // The next start code begins 3 bytes before its payload; trailing zeros
            // belong to a 4-byte start code (or are padding), never to this unit.
            Some(&next) => trim_trailing_zeros(&data[start..next - 3]),
            None => trim_trailing_zeros(&data[start..]),
        })
        .filter(|nal| !nal.is_empty())
        .collect()
}

fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let mut end = nal.len();
    while end > 0 && nal[end - 1] == 0 {
        end -= 1;
    }
    &nal[..end]
}

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map_or(0, |b| b & 0x1f)
}

/// Converts one Annex B access unit to AVCC, pulling out parameter sets and AUDs.
pub fn to_access_unit(annexb: &[u8]) -> AccessUnit {
    let mut unit = AccessUnit { is_key: false, avcc: Vec::with_capacity(annexb.len()), sps: None, pps: None };
    for nal in nal_units(annexb) {
        match nal_type(nal) {
            NAL_SPS => unit.sps = Some(nal.to_vec()),
            NAL_PPS => unit.pps = Some(nal.to_vec()),
            NAL_AUD => {}
            kind => {
                unit.is_key |= kind == NAL_IDR;
                unit.avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
                unit.avcc.extend_from_slice(nal);
            }
        }
    }
    unit
}

/// AVCDecoderConfigurationRecord for one SPS and one PPS (8-bit 4:2:0, as encoded from NV12).
pub fn avcc_record(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() < 4 || pps.is_empty() || sps.len() > u16::MAX as usize || pps.len() > u16::MAX as usize {
        return None;
    }
    let profile = sps[1];
    let mut out = vec![1, profile, sps[2], sps[3], 0xff, 0xe1];
    out.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    out.extend_from_slice(sps);
    out.push(1);
    out.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    out.extend_from_slice(pps);
    if HIGH_PROFILES.contains(&profile) {
        // chroma_format_idc = 1 (4:2:0), 8-bit luma and chroma, no SPS extensions.
        out.extend_from_slice(&[0xfc | 1, 0xf8, 0xf8, 0]);
    }
    Some(out)
}

/// RFC 6381 codec string ("avc1.PPCCLL") for WebCodecs.
pub fn codec_string(avcc: &[u8]) -> Option<String> {
    if avcc.len() < 4 || avcc[0] != 1 {
        return None;
    }
    Some(format!("avc1.{:02x}{:02x}{:02x}", avcc[1], avcc[2], avcc[3]))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 6] = [0x67, 0x64, 0x00, 0x1f, 0xac, 0xd9];
    const PPS: [u8; 4] = [0x68, 0xeb, 0xe3, 0xcb];

    fn annexb(units: &[&[u8]]) -> Vec<u8> {
        units.iter().flat_map(|u| [&[0, 0, 0, 1][..], u].concat()).collect()
    }

    #[test]
    fn splits_three_and_four_byte_start_codes() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5];
        let units = nal_units(&data);
        assert_eq!(units, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5][..]]);
    }

    #[test]
    fn ignores_leading_garbage_and_empty_units() {
        assert!(nal_units(&[1, 2, 3]).is_empty());
        assert_eq!(nal_units(&[9, 0, 0, 1, 0x41, 7, 0, 0, 1]), vec![&[0x41, 7][..]]);
    }

    #[test]
    fn key_frame_becomes_length_prefixed_without_parameter_sets() {
        let unit = to_access_unit(&annexb(&[&[0x09, 0x10], &SPS, &PPS, &[0x65, 0xaa, 0xbb]]));
        assert!(unit.is_key);
        assert_eq!(unit.sps.as_deref(), Some(&SPS[..]));
        assert_eq!(unit.pps.as_deref(), Some(&PPS[..]));
        assert_eq!(unit.avcc, vec![0, 0, 0, 3, 0x65, 0xaa, 0xbb]);
    }

    #[test]
    fn delta_frame_is_not_key() {
        let unit = to_access_unit(&annexb(&[&[0x41, 1], &[0x06, 5]]));
        assert!(!unit.is_key);
        assert_eq!(unit.avcc, vec![0, 0, 0, 2, 0x41, 1, 0, 0, 0, 2, 0x06, 5]);
        assert!(unit.sps.is_none());
    }

    #[test]
    fn builds_avcc_with_high_profile_extension() {
        let rec = avcc_record(&SPS, &PPS).unwrap();
        assert_eq!(&rec[..8], &[1, 0x64, 0x00, 0x1f, 0xff, 0xe1, 0, 6]);
        assert_eq!(&rec[8..14], &SPS);
        assert_eq!(&rec[14..17], &[1, 0, 4]);
        assert_eq!(&rec[17..21], &PPS);
        assert_eq!(&rec[21..], &[0xfd, 0xf8, 0xf8, 0]);
        assert_eq!(codec_string(&rec).unwrap(), "avc1.64001f");
    }

    #[test]
    fn main_profile_has_no_extension() {
        let sps = [0x67, 0x4d, 0x40, 0x28];
        let rec = avcc_record(&sps, &PPS).unwrap();
        assert_eq!(rec.len(), 6 + 2 + 4 + 1 + 2 + 4);
        assert_eq!(codec_string(&rec).unwrap(), "avc1.4d4028");
    }

    #[test]
    fn rejects_malformed_inputs() {
        assert!(avcc_record(&[0x67, 1], &PPS).is_none());
        assert!(avcc_record(&SPS, &[]).is_none());
        assert!(codec_string(&[0, 1, 2, 3]).is_none());
        assert!(codec_string(&[1, 2]).is_none());
        assert_eq!(nal_type(&[]), 0);
    }
}
