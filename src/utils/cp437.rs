const HIGH: [char; 128] = [
    'Ç', 'ü', 'é', 'â', 'ä', 'à', 'å', 'ç', 'ê', 'ë', 'è', 'ï', 'î', 'ì', 'Ä', 'Å', 'É', 'æ', 'Æ', 'ô', 'ö', 'ò', 'û', 'ù', 'ÿ', 'Ö', 'Ü', '¢', '£', '¥', '₧',
    'ƒ', 'á', 'í', 'ó', 'ú', 'ñ', 'Ñ', 'ª', 'º', '¿', '⌐', '¬', '½', '¼', '¡', '«', '»', '░', '▒', '▓', '│', '┤', '╡', '╢', '╖', '╕', '╣', '║', '╗', '╝', '╜',
    '╛', '┐', '└', '┴', '┬', '├', '─', '┼', '╞', '╟', '╚', '╔', '╩', '╦', '╠', '═', '╬', '╧', '╨', '╤', '╥', '╙', '╘', '╒', '╓', '╫', '╪', '┘', '┌', '█', '▄',
    '▌', '▐', '▀', 'α', 'ß', 'Γ', 'π', 'Σ', 'σ', 'µ', 'τ', 'Φ', 'Θ', 'Ω', 'δ', '∞', 'φ', 'ε', '∩', '≡', '±', '≥', '≤', '⌠', '⌡', '÷', '≈', '°', '∙', '·', '√',
    'ⁿ', '²', '■', '\u{a0}',
];

pub fn decode(bytes: &[u8]) -> String {
    if bytes.is_ascii() {
        return String::from_utf8(bytes.to_vec()).expect("ascii is valid utf-8");
    }

    let mut s = String::with_capacity(bytes.len());
    for &b in bytes {
        if b < 0x80 {
            s.push(b as char);
        } else {
            s.push(HIGH[(b - 0x80) as usize]);
        }
    }
    s
}

pub fn encode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    for ch in s.chars() {
        if (ch as u32) < 0x80 {
            out.push(ch as u8);
        } else {
            let idx = HIGH.iter().position(|&c| c == ch)?;
            out.push(0x80 + idx as u8);
        }
    }
    Some(out)
}

pub fn decode_name(bytes: &[u8], utf8_flag: bool) -> String {
    if utf8_flag {
        match std::str::from_utf8(bytes) {
            Ok(s) => return s.to_owned(),
            Err(_) => return decode(bytes),
        }
    }
    decode(bytes)
}
