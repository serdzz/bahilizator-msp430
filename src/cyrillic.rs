//! Turning Rust text into the bytes a Russian character display expects.
//!
//! The HD44780 has no one character set. The Russian parts these machines use — МЭЛТ MT-16S2H and
//! the Winstar WH1602B in its Cyrillic build, among others — share a table in which the Cyrillic
//! letters that are drawn identically to a Latin one are simply the Latin code (А is `A`, Р is `P`,
//! С is `C`), and the thirty-odd that are not live above 0xA0.
//!
//! **This table is the common one, not a universal one.** A display with a different character ROM
//! will draw the right ASCII and the wrong Cyrillic, and the only way to know is to look at the
//! fitted part. That is worth a minute with a datasheet before the first production run.

/// The byte that draws `c`, or `None` if this character set has no such glyph.
pub const fn encode(c: char) -> Option<u8> {
    // ASCII is ASCII on every part.
    if (c as u32) < 0x80 {
        return Some(c as u8);
    }

    Some(match c {
        // Uppercase, in alphabetical order.
        'А' => b'A',
        'Б' => 0xa0,
        'В' => b'B',
        'Г' => 0xa1,
        'Д' => 0xe0,
        'Е' => b'E',
        'Ё' => 0xa2,
        'Ж' => 0xa3,
        'З' => 0xa4,
        'И' => 0xa5,
        'Й' => 0xa6,
        'К' => b'K',
        'Л' => 0xa7,
        'М' => b'M',
        'Н' => b'H',
        'О' => b'O',
        'П' => 0xa8,
        'Р' => b'P',
        'С' => b'C',
        'Т' => b'T',
        'У' => 0xa9,
        'Ф' => 0xaa,
        'Х' => b'X',
        'Ц' => 0xe1,
        'Ч' => 0xab,
        'Ш' => 0xac,
        'Щ' => 0xe2,
        'Ъ' => 0xad,
        'Ы' => 0xae,
        'Ь' => 0xaf,
        'Э' => 0xb0,
        'Ю' => 0xb1,
        'Я' => 0xb2,

        // Lowercase.
        'а' => b'a',
        'б' => 0xb3,
        'в' => 0xb4,
        'г' => 0xb5,
        'д' => 0xe3,
        'е' => b'e',
        'ё' => 0xb6,
        'ж' => 0xb7,
        'з' => 0xb8,
        'и' => 0xb9,
        'й' => 0xba,
        'к' => 0xbb,
        'л' => 0xbc,
        'м' => 0xbd,
        'н' => 0xbe,
        'о' => b'o',
        'п' => 0xbf,
        'р' => b'p',
        'с' => b'c',
        'т' => 0xc0,
        'у' => b'y',
        'ф' => 0xe4,
        'х' => b'x',
        'ц' => 0xe5,
        'ч' => 0xc1,
        'ш' => 0xc2,
        'щ' => 0xe6,
        'ъ' => 0xc3,
        'ы' => 0xc4,
        'ь' => 0xc5,
        'э' => 0xc6,
        'ю' => 0xc7,
        'я' => 0xc8,

        _ => return None,
    })
}

/// The byte that draws `c`, falling back to a question mark.
///
/// Drawing the wrong character is better than drawing nothing: a missing glyph silently shortens
/// the line and moves everything after it, which makes a display look broken rather than
/// incomplete.
pub const fn encode_or_replacement(c: char) -> u8 {
    match encode(c) {
        Some(b) => b,
        None => b'?',
    }
}
