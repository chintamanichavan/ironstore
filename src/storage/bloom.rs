// Bloom filter for SSTable membership tests.
// Parameterised for ~1% false-positive rate: 10 bits/element, 7 hash functions.
// Uses double-hashing over FNV-1a so no external dependency is needed.

const BITS_PER_ELEMENT: usize = 10;
const NUM_HASHES: usize = 7;

pub struct BloomFilter {
    bits: Vec<u8>,
    m: usize,
}

impl BloomFilter {
    pub fn new(n: usize) -> Self {
        let m = (n * BITS_PER_ELEMENT).max(64);
        let byte_len = (m + 7) / 8;
        Self {
            bits: vec![0u8; byte_len],
            m,
        }
    }

    pub fn insert(&mut self, key: &[u8]) {
        let (h1, h2) = hash_pair(key);
        for i in 0..NUM_HASHES {
            let bit = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.m as u64;
            self.bits[bit as usize / 8] |= 1 << (bit % 8);
        }
    }

    /// Returns false only when the key is definitely absent.
    pub fn might_contain(&self, key: &[u8]) -> bool {
        let (h1, h2) = hash_pair(key);
        for i in 0..NUM_HASHES {
            let bit = h1.wrapping_add((i as u64).wrapping_mul(h2)) % self.m as u64;
            if self.bits[bit as usize / 8] & (1 << (bit % 8)) == 0 {
                return false;
            }
        }
        true
    }

    // Serialisation: [m: u32 LE][bits: bytes]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(4 + self.bits.len());
        out.extend_from_slice(&(self.m as u32).to_le_bytes());
        out.extend_from_slice(&self.bits);
        out
    }

    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        let m = u32::from_le_bytes(data[0..4].try_into().ok()?) as usize;
        let byte_len = (m + 7) / 8;
        if data.len() < 4 + byte_len {
            return None;
        }
        Some(Self {
            bits: data[4..4 + byte_len].to_vec(),
            m,
        })
    }
}

fn hash_pair(key: &[u8]) -> (u64, u64) {
    (fnv1a(key, 0), fnv1a(key, 0x9e3779b97f4a7c15))
}

fn fnv1a(data: &[u8], seed: u64) -> u64 {
    let mut h: u64 = 14695981039346656037u64.wrapping_add(seed);
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_false_negatives() {
        let mut bf = BloomFilter::new(100);
        let keys: Vec<Vec<u8>> = (0..100).map(|i| format!("key{i}").into_bytes()).collect();
        for k in &keys {
            bf.insert(k);
        }
        for k in &keys {
            assert!(bf.might_contain(k), "false negative for {k:?}");
        }
    }

    #[test]
    fn round_trips_serialisation() {
        let mut bf = BloomFilter::new(50);
        bf.insert(b"hello");
        bf.insert(b"world");
        let bytes = bf.to_bytes();
        let bf2 = BloomFilter::from_bytes(&bytes).unwrap();
        assert!(bf2.might_contain(b"hello"));
        assert!(bf2.might_contain(b"world"));
        assert!(!bf2.might_contain(b"nope_definitely_absent_xyzzy"));
    }
}
