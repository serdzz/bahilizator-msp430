//! Turning Rust text into the bytes a Russian character display expects.
//!
//! The HD44780 has no one character set. The Russian parts these machines use — МЭЛТ MT-16S2H and
//! the Winstar WH1602B in its Cyrillic build, among others — share a table in which the Cyrillic
//! letters that are drawn identically to a Latin one are simply the Latin code (А is `A`, Р is `P`,
//! С is `C`), and the thirty-odd that are not live above 0xA0.
//!
//! **This table is transcribed from the C original's `cyrillic_chars[]` (`lcd.c:14-21`)**, a
//! 64-entry array covering input bytes 0xC0-0xFF (`LcdPrint`, `lcd.c:157-168`:
//! `cyrillic_chars[text[c]-0xC0]`) in the order А,Б,В,…,Я,а,б,в,…,я — standard Windows-1251
//! ordering for that range, with no separate entries for Ё/ё. A byte-for-byte comparison against
//! that table found the Rust values from Ь (0xDC) onward were shifted one ordinal position too far
//! (PORT_AUDIT.md §4): Ь itself was assigned the next *ordinal* code (0xAF) instead of the C
//! table's actual special-cased value (0x62 — Ь is drawn as a lowercase Latin 'b' on this
//! character ROM), which pushed every Cyrillic-specific code after it — Э, Ю, Я and всё
//! lowercase from б through я — one slot later than the C original. The values below match the C
//! table exactly for every letter it defines.
//!
//! Ё and ё are not in the C original's table at all — the audit's own reading of `lcd.c` found no
//! entries for them — so this port's codes for those two are its own choice, not a transcription.
//! `Ё` uses 0xA2 and `ё` uses 0xB5, the two ordinal values the C table leaves unused in the 0xA0-0xC7
//! run (`0xA2` between Б/Г and `0xB5` between г/ж), which keeps them from colliding with any letter
//! the C table *does* define — an earlier version of this fix would have put `ё` at 0xB6, which is
//! Ж's lowercase 'ж' in the corrected table, and the two would have drawn identically.
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
        'Ь' => 0x62,
        'Э' => 0xaf,
        'Ю' => 0xb0,
        'Я' => 0xb1,

        // Lowercase.
        'а' => b'a',
        'б' => 0xb2,
        'в' => 0xb3,
        'г' => 0xb4,
        'д' => 0xe3,
        'е' => b'e',
        'ё' => 0xb5,
        'ж' => 0xb6,
        'з' => 0xb7,
        'и' => 0xb8,
        'й' => 0xb9,
        'к' => 0xba,
        'л' => 0xbb,
        'м' => 0xbc,
        'н' => 0xbd,
        'о' => b'o',
        'п' => 0xbe,
        'р' => b'p',
        'с' => b'c',
        'т' => 0xbf,
        'у' => b'y',
        'ф' => 0xe4,
        'х' => b'x',
        'ц' => 0xe5,
        'ч' => 0xc0,
        'ш' => 0xc1,
        'щ' => 0xe6,
        'ъ' => 0xc2,
        'ы' => 0xc3,
        'ь' => 0xc4,
        'э' => 0xc5,
        'ю' => 0xc6,
        'я' => 0xc7,

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
