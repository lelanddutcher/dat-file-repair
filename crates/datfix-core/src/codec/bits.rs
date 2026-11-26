//! Bit-level reading for H.264/HEVC headers.
//!
//! Parameter sets and slice headers are packed bit fields, many of them Exp-Golomb coded
//! ("ue(v)"/"se(v)" in the specifications). Before parsing, the *emulation prevention* bytes
//! (`00 00 03` → `00 00`) that the encoder inserted must be removed.

/// Removes emulation prevention bytes, turning a NAL unit payload into its raw bit string.
pub fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// Reads bits most-significant first.
pub struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, bit: 0 }
    }

    fn end() -> String {
        "header ended early".to_string()
    }

    /// Bits read so far.
    pub fn bit_pos(&self) -> usize {
        self.bit
    }

    /// One bit.
    pub fn flag(&mut self) -> Result<bool, String> {
        let byte = *self.data.get(self.bit / 8).ok_or_else(Self::end)?;
        let v = (byte >> (7 - self.bit % 8)) & 1;
        self.bit += 1;
        Ok(v == 1)
    }

    /// `n` bits as an unsigned number (`n` ≤ 32).
    pub fn u(&mut self, n: u32) -> Result<u32, String> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.flag()? as u32;
        }
        Ok(v)
    }

    /// Skips `n` bits.
    pub fn skip(&mut self, n: usize) -> Result<(), String> {
        if self.bit + n > self.data.len() * 8 {
            return Err(Self::end());
        }
        self.bit += n;
        Ok(())
    }

    /// Unsigned Exp-Golomb code.
    pub fn ue(&mut self) -> Result<u32, String> {
        let mut zeros = 0;
        while !self.flag()? {
            zeros += 1;
            // 31 or more leading zeros would code values of 2^31 and up, which no header field
            // this crate reads can hold. Rejecting them keeps all later arithmetic in range.
            if zeros > 30 {
                return Err("bad Exp-Golomb code".into());
            }
        }
        Ok(((1u64 << zeros) - 1 + self.u(zeros)? as u64) as u32)
    }

    /// Signed Exp-Golomb code.
    pub fn se(&mut self) -> Result<i32, String> {
        let k = self.ue()? as i64;
        Ok(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) } as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb() {
        // Bits: 1 | 010 | 011 | 00100 | 00101  => ue 0, 1, 2, 3, se(4) = -2
        let data = [0b1010_0110, 0b0100_0010, 0b1000_0000];
        let mut r = BitReader::new(&data);
        assert_eq!(r.ue().unwrap(), 0);
        assert_eq!(r.ue().unwrap(), 1);
        assert_eq!(r.ue().unwrap(), 2);
        assert_eq!(r.ue().unwrap(), 3);
        assert_eq!(r.se().unwrap(), -2);
    }

    #[test]
    fn removes_emulation_prevention() {
        assert_eq!(unescape(&[0, 0, 3, 1, 0, 0, 3, 0]), vec![0, 0, 1, 0, 0, 0]);
    }
}
