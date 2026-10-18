//! ICC profile が sRGB の matrix/TRC 型か。説明 ('desc') は名乗りでしかないので判定に使わない。
//! 色の変換はしない。sRGB 以外は読み込みを拒否するための検査。

const SRGB_COLORANTS: [[f64; 3]; 3] = [
    [0.436066, 0.222488, 0.013916],
    [0.385147, 0.716873, 0.097076],
    [0.143066, 0.060608, 0.714096],
];
const WHITE_D50: [f64; 3] = [0.964203, 1.0, 0.824905];
const WHITE_D65: [f64; 3] = [0.950455, 1.0, 1.089050];
const XYZ_TOLERANCE: f64 = 0.003;
const CURVE_TOLERANCE: f64 = 0.002;
const CURVE_SAMPLES: i32 = 1024;
const LUT_TAGS: [&[u8]; 14] = [
    b"A2B0", b"A2B1", b"A2B2", b"B2A0", b"B2A1", b"B2A2", b"D2B0", b"D2B1", b"D2B2", b"D2B3",
    b"B2D0", b"B2D1", b"B2D2", b"B2D3",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IccRejection {
    pub description: String,
    pub reason: String,
}

/// sRGB なら `Ok`。違う、または壊れているなら説明と理由。
pub fn icc_profile_is_srgb(data: &[u8]) -> Result<(), IccRejection> {
    if data.len() < 132 {
        return Err(reject("", "profile が短すぎます"));
    }
    let declared = be32(data) as usize;
    if declared < 132 || declared > data.len() {
        return Err(reject("", "profile の大きさが不正です"));
    }
    let data = &data[..declared];
    let description = icc_description(data);
    if &data[12..16] != b"mntr" {
        return Err(reject(
            description,
            "表示装置用 (mntr) の profile ではありません",
        ));
    }
    if &data[16..20] != b"RGB " || &data[20..24] != b"XYZ " {
        return Err(reject(description, "RGB / XYZ の profile ではありません"));
    }
    for lut in LUT_TAGS {
        match find_tag(data, lut) {
            Ok(Some(_)) => {
                return Err(reject(
                    description,
                    format!("LUT 型の profile ({}) は検証できません", tag_name(lut)),
                ));
            }
            Err(()) => {
                return Err(reject(
                    description,
                    format!("LUT 型の profile ({}) は検証できません", tag_name(lut)),
                ));
            }
            Ok(None) => {}
        }
    }
    for (channel, name) in ["rXYZ", "gXYZ", "bXYZ"].into_iter().enumerate() {
        let Some(tag) = find_tag(data, name.as_bytes())
            .map_err(|_| reject(description.clone(), format!("原色 ({name}) を読めません")))?
        else {
            return Err(reject(description, format!("原色 ({name}) を読めません")));
        };
        let Some(xyz) = read_xyz(tag) else {
            return Err(reject(description, format!("原色 ({name}) を読めません")));
        };
        if !near_xyz(xyz, SRGB_COLORANTS[channel]) {
            return Err(reject(
                description,
                format!("原色 ({name}) が sRGB と違います"),
            ));
        }
    }
    let Some(white) = find_tag(data, b"wtpt")
        .map_err(|_| reject(description.clone(), "白色点 (wtpt) を読めません"))?
    else {
        return Err(reject(description, "白色点 (wtpt) を読めません"));
    };
    let Some(xyz) = read_xyz(white) else {
        return Err(reject(description, "白色点 (wtpt) を読めません"));
    };
    if !near_xyz(xyz, WHITE_D50) && !near_xyz(xyz, WHITE_D65) {
        return Err(reject(
            description,
            "白色点 (wtpt) が D50 / D65 ではありません",
        ));
    }
    for name in ["rTRC", "gTRC", "bTRC"] {
        let Some(tag) = find_tag(data, name.as_bytes()).map_err(|_| {
            reject(
                description.clone(),
                format!("トーンカーブ ({name}) が sRGB と違います"),
            )
        })?
        else {
            return Err(reject(
                description,
                format!("トーンカーブ ({name}) が sRGB と違います"),
            ));
        };
        if !curve_is_srgb(tag) {
            return Err(reject(
                description,
                format!("トーンカーブ ({name}) が sRGB と違います"),
            ));
        }
    }
    Ok(())
}

fn reject(description: impl Into<String>, reason: impl Into<String>) -> IccRejection {
    IccRejection {
        description: description.into(),
        reason: reason.into(),
    }
}

fn tag_name(tag: &[u8]) -> String {
    String::from_utf8_lossy(tag).into_owned()
}

fn find_tag<'a>(data: &'a [u8], signature: &[u8]) -> Result<Option<&'a [u8]>, ()> {
    if data.len() < 132 || signature.len() != 4 {
        return Err(());
    }
    let count = be32(&data[128..]) as usize;
    if count > (data.len() - 132) / 12 {
        return Err(());
    }
    for index in 0..count {
        let entry = 132 + index * 12;
        if &data[entry..entry + 4] != signature {
            continue;
        }
        let offset = be32(&data[entry + 4..]) as usize;
        let length = be32(&data[entry + 8..]) as usize;
        if offset > data.len() || length > data.len() - offset || length < 8 {
            return Err(());
        }
        return Ok(Some(&data[offset..offset + length]));
    }
    Ok(None)
}

fn icc_description(data: &[u8]) -> String {
    let Ok(Some(tag)) = find_tag(data, b"desc") else {
        return String::new();
    };
    if tag.len() < 12 {
        return String::new();
    }
    if &tag[0..4] == b"desc" {
        let count = be32(&tag[8..]) as usize;
        let mut text = String::new();
        for index in 0..count {
            if 12 + index >= tag.len() || tag[12 + index] == 0 {
                break;
            }
            text.push(tag[12 + index] as char);
        }
        return text;
    }
    if &tag[0..4] == b"mluc" && tag.len() >= 28 && be32(&tag[8..]) > 0 {
        let bytes = be32(&tag[20..]) as usize;
        let start = be32(&tag[24..]) as usize;
        let mut text = String::new();
        let mut index = 0;
        while index + 1 < bytes && start + index + 1 < tag.len() {
            let high = tag[start + index];
            let low = tag[start + index + 1];
            if high == 0 && low != 0 && low < 0x80 {
                text.push(low as char);
            }
            index += 2;
        }
        return text;
    }
    String::new()
}

fn read_xyz(tag: &[u8]) -> Option<[f64; 3]> {
    if tag.len() < 20 || &tag[0..4] != b"XYZ " {
        return None;
    }
    Some([
        s15fixed16(&tag[8..]),
        s15fixed16(&tag[12..]),
        s15fixed16(&tag[16..]),
    ])
}

fn near_xyz(value: [f64; 3], expected: [f64; 3]) -> bool {
    value
        .iter()
        .zip(expected)
        .all(|(left, right)| (left - right).abs() <= XYZ_TOLERANCE)
}

fn curve_is_srgb(tag: &[u8]) -> bool {
    if tag.len() < 12 {
        return false;
    }
    if &tag[0..4] == b"curv" {
        let count = be32(&tag[8..]) as usize;
        if count < 2 || count > (tag.len() - 12) / 2 {
            return false;
        }
        for index in 0..count {
            let x = index as f64 / (count - 1) as f64;
            let y = f64::from(be16(&tag[12 + index * 2..])) / 65535.0;
            if (y - srgb_to_linear(x)).abs() > CURVE_TOLERANCE {
                return false;
            }
        }
        return true;
    }
    if &tag[0..4] == b"para" {
        let counts = [1, 3, 4, 5, 7];
        let kind = be16(&tag[8..]) as usize;
        if kind > 4 {
            return false;
        }
        let count = counts[kind];
        if tag.len() < 12 + count * 4 {
            return false;
        }
        let mut parameters = [0.0; 7];
        for index in 0..count {
            parameters[index] = s15fixed16(&tag[12 + index * 4..]);
        }
        if (kind == 1 || kind == 2) && parameters[1] == 0.0 {
            return false;
        }
        for index in 0..CURVE_SAMPLES {
            let x = f64::from(index) / f64::from(CURVE_SAMPLES - 1);
            if (evaluate_parametric(kind, parameters, x) - srgb_to_linear(x)).abs()
                > CURVE_TOLERANCE
            {
                return false;
            }
        }
        return true;
    }
    false
}

fn evaluate_parametric(kind: usize, parameters: [f64; 7], x: f64) -> f64 {
    let power = |base: f64| {
        if base > 0.0 {
            base.powf(parameters[0])
        } else {
            0.0
        }
    };
    match kind {
        0 => power(x),
        1 => {
            if x >= -parameters[2] / parameters[1] {
                power(parameters[1] * x + parameters[2])
            } else {
                0.0
            }
        }
        2 => {
            if x >= -parameters[2] / parameters[1] {
                power(parameters[1] * x + parameters[2]) + parameters[3]
            } else {
                parameters[3]
            }
        }
        3 => {
            if x >= parameters[4] {
                power(parameters[1] * x + parameters[2])
            } else {
                parameters[3] * x
            }
        }
        _ => {
            if x >= parameters[4] {
                power(parameters[1] * x + parameters[2]) + parameters[5]
            } else {
                parameters[3] * x + parameters[6]
            }
        }
    }
}

fn srgb_to_linear(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn be16(bytes: &[u8]) -> u16 {
    u16::from(bytes[0]) << 8 | u16::from(bytes[1])
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) << 24
        | u32::from(bytes[1]) << 16
        | u32::from(bytes[2]) << 8
        | u32::from(bytes[3])
}

fn s15fixed16(bytes: &[u8]) -> f64 {
    f64::from(be32(bytes) as i32) / 65536.0
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn be_bytes(value: u32) -> [u8; 4] {
        value.to_be_bytes()
    }

    fn fixed(value: f64) -> [u8; 4] {
        be_bytes((value * 65536.0).round() as i32 as u32)
    }

    fn xyz_tag(xyz: [f64; 3]) -> Vec<u8> {
        let mut tag = b"XYZ ".to_vec();
        tag.extend_from_slice(&[0, 0, 0, 0]);
        for value in xyz {
            tag.extend_from_slice(&fixed(value));
        }
        tag
    }

    fn srgb_trc() -> Vec<u8> {
        let mut tag = b"para".to_vec();
        tag.extend_from_slice(&[0, 0, 0, 0, 0, 3, 0, 0]);
        for value in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            tag.extend_from_slice(&fixed(value));
        }
        tag
    }

    fn profile(description: &str, colorants: [[f64; 3]; 3]) -> Vec<u8> {
        let ascii = description.as_bytes();
        let mut desc = b"desc".to_vec();
        desc.extend_from_slice(&[0, 0, 0, 0]);
        desc.extend_from_slice(&be_bytes((ascii.len() + 1) as u32));
        desc.extend_from_slice(ascii);
        desc.push(0);
        desc.extend_from_slice(&[0u8; 78]);
        let trc = srgb_trc();
        let named = [
            ("desc", desc),
            ("wtpt", xyz_tag(WHITE_D65)),
            ("rXYZ", xyz_tag(colorants[0])),
            ("gXYZ", xyz_tag(colorants[1])),
            ("bXYZ", xyz_tag(colorants[2])),
            ("rTRC", trc),
        ];
        let aliases = ["gTRC", "bTRC"];
        let entry_count = named.len() + aliases.len();
        let table_size = 4 + 12 * entry_count;
        let mut body = Vec::new();
        let mut placed = Vec::new();
        for (name, tag) in &named {
            placed.push(((*name), 128 + table_size + body.len(), tag.len()));
            body.extend_from_slice(tag);
            while body.len() % 4 != 0 {
                body.push(0);
            }
        }
        let mut table = be_bytes(entry_count as u32).to_vec();
        for (name, offset, length) in &placed {
            table.extend_from_slice(name.as_bytes());
            table.extend_from_slice(&be_bytes(*offset as u32));
            table.extend_from_slice(&be_bytes(*length as u32));
        }
        let (_, trc_offset, trc_length) = placed[5];
        for name in aliases {
            table.extend_from_slice(name.as_bytes());
            table.extend_from_slice(&be_bytes(trc_offset as u32));
            table.extend_from_slice(&be_bytes(trc_length as u32));
        }
        let mut bytes = be_bytes((128 + table_size + body.len()) as u32).to_vec();
        bytes.extend_from_slice(&[0, 0, 0, 0, 0x02, 0x10, 0x00, 0x00]);
        bytes.extend_from_slice(b"mntrRGB XYZ ");
        bytes.resize(128, 0);
        bytes.extend_from_slice(&table);
        bytes.extend_from_slice(&body);
        bytes
    }

    #[test]
    fn accepts_an_srgb_matrix_profile_and_rejects_a_shifted_primary() {
        let srgb = profile("sRGB IEC61966-2.1", SRGB_COLORANTS);
        assert!(icc_profile_is_srgb(&srgb).is_ok(), "{srgb:?}");
        let mut wide = SRGB_COLORANTS;
        wide[0][0] += 0.08;
        let shifted = profile("sRGB compatible profile", wide);
        let rejected = icc_profile_is_srgb(&shifted).expect_err("広色域は拒否する");
        assert!(rejected.reason.contains("原色"));
        assert!(rejected.description.contains("sRGB"));
    }

    #[test]
    fn rejects_a_short_profile_and_a_lut_tag() {
        assert!(icc_profile_is_srgb(&[0, 1, 2]).is_err());
        let mut srgb = profile("sRGB", SRGB_COLORANTS);
        // tag count を 1 増やし、A2B0 を空の範囲外ではなく既存 desc へ向ける。
        let count = be32(&srgb[128..]);
        srgb[128..132].copy_from_slice(&be_bytes(count + 1));
        // 表が 12 byte 足りないので、この変更だけでは壊れた profile になる。
        // 短い buffer の拒否は上で見て、LUT は tag を正しく足した profile で見る。
        let _ = srgb;
        let with_lut = profile_with_lut();
        let rejected = icc_profile_is_srgb(&with_lut).expect_err("LUT は拒否する");
        assert!(rejected.reason.contains("LUT"));
    }

    fn profile_with_lut() -> Vec<u8> {
        let mut bytes = profile("Display P3", SRGB_COLORANTS);
        // 既存の tag count の直後に入る余地は無いので、A2B0 という signature を
        // 使った最小 profile を header 検査の後に置く。rXYZ の tag 名を A2B0 に書き換えると
        // 原色が読めなくなるより先に LUT として拒否される。
        let count = be32(&bytes[128..]) as usize;
        for index in 0..count {
            let entry = 132 + index * 12;
            if &bytes[entry..entry + 4] == b"rXYZ" {
                bytes[entry..entry + 4].copy_from_slice(b"A2B0");
                break;
            }
        }
        bytes
    }
}
