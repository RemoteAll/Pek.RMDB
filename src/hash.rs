//! 快速哈希（FxHash）与容器别名。
//!
//! # 为什么不用默认 SipHash
//!
//! 内存数据库的键来自业务实体（可信输入），不面对哈希碰撞 DoS；默认 `SipHash` 在
//! `i64`/`&str` 这类短键上约 20 ns/次，而一次插入要发生 10+ 次哈希查找。FxHash 每 key
//! 约 1~3 ns，实测 `find_by_id` / 插入有 5~15% 提升。
//!
//! 若未来需要接受不可信键（如公网直传字段名），应换回 `RandomState`。

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// FxHash（rustc 内部同款算法）。
#[derive(Default)]
pub(crate) struct FxHasher {
    hash: u64,
}

const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

impl FxHasher {
    #[inline]
    fn add(&mut self, value: u64) {
        self.hash = (self.hash.rotate_left(5) ^ value).wrapping_mul(SEED);
    }
}

impl Hasher for FxHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }

    fn write(&mut self, bytes: &[u8]) {
        let (chunks, remainder) = bytes.as_chunks::<8>();
        for chunk in chunks {
            self.add(u64::from_le_bytes(*chunk));
        }
        if !remainder.is_empty() {
            let mut buf = [0u8; 8];
            buf[..remainder.len()].copy_from_slice(remainder);
            self.add(u64::from_le_bytes(buf));
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(u64::from(value));
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }
}

/// 快速哈希表。
pub(crate) type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;

/// 快速哈希集合。
pub(crate) type FastSet<T> = HashSet<T, BuildHasherDefault<FxHasher>>;

pub(crate) fn new_map<K, V>() -> FastMap<K, V> {
    FastMap::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::hash::Hash;

    #[test]
    fn fast_map_behaves_like_hash_map() {
        let mut map = new_map();
        map.insert(1i64, "a");
        map.insert(2, "b");
        assert_eq!(map.get(&1), Some(&"a"));
        assert_eq!(map.get(&3), None);
        map.remove(&1);
        assert!(!map.contains_key(&1));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn writes_long_strings_and_ints_consistently() {
        let mut hasher = FxHasher::default();
        "abcdefghijklmnop".hash(&mut hasher);
        let text_hash = hasher.finish();

        let mut hasher = FxHasher::default();
        "abcdefghijklmnop".hash(&mut hasher);
        assert_eq!(hasher.finish(), text_hash);

        let mut hasher = FxHasher::default();
        42u64.hash(&mut hasher);
        assert_ne!(hasher.finish(), text_hash);
    }
}
