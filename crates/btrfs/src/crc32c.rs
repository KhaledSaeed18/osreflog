//! CRC-32C (Castagnoli) as btrfs uses it for send-stream commands: the raw update function,
//! seeded with 0 and without a final inversion (the kernel's `crc32c(0, buf, len)`).

const POLY: u32 = 0x82F6_3B78;

const TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        #[allow(clippy::cast_possible_truncation)]
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ POLY
            } else {
                crc >> 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Continues a raw CRC-32C over `data`.
#[must_use]
pub fn update(mut crc: u32, data: &[u8]) -> u32 {
    for &byte in data {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xff) as usize] ^ (crc >> 8);
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::update;

    #[test]
    fn matches_the_standard_check_value() {
        // The standard CRC-32C ("123456789") inverts before and after.
        assert_eq!(!update(!0, b"123456789"), 0xE306_9283);
    }

    #[test]
    fn is_incremental() {
        let whole = update(0, b"btrfs-stream");
        let parts = update(update(0, b"btrfs-"), b"stream");
        assert_eq!(whole, parts);
    }
}
