//! SplitMix64 — a small deterministic pseudo-random generator with no
//! dependencies.
//!
//! Every seeded choice in this crate draws from one of these, in a fixed
//! order. The algorithm is part of the determinism contract: changing it
//! changes every generated tree, so it changes only as a deliberate,
//! reviewable act.

/// A seeded generator. Cheap to construct, cheap to copy a seed into.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `0..n`. Panics if `n == 0`.
    pub fn range(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }

    /// Uniform value in `min..=max`. Panics if `max < min`.
    pub fn between(&mut self, min: usize, max: usize) -> usize {
        assert!(max >= min, "between() requires max >= min");
        min + self.range(max - min + 1)
    }

    /// True with probability `num`/1000.
    pub fn per_mille(&mut self, num: u32) -> bool {
        (self.next_u64() % 1000) < u64::from(num)
    }

    /// Pick a uniformly random element from a non-empty slice.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.range(items.len())]
    }

    /// Pick an index from `weights`, proportional to each entry's weight.
    /// Panics if the weights sum to zero.
    pub fn weighted(&mut self, weights: &[u32]) -> usize {
        let total: u64 = weights.iter().map(|w| u64::from(*w)).sum();
        assert!(total > 0, "weighted() requires a nonzero weight sum");
        let mut draw = self.next_u64() % total;
        for (index, weight) in weights.iter().enumerate() {
            let weight = u64::from(*weight);
            if draw < weight {
                return index;
            }
            draw -= weight;
        }
        weights.len() - 1
    }

    /// Move `count` distinct elements of `pool` to its front, in draw order.
    /// A partial Fisher-Yates: the caller reads `pool[..count]`. Panics if
    /// `count` exceeds the pool.
    pub fn take_distinct<T>(&mut self, pool: &mut [T], count: usize) {
        assert!(count <= pool.len(), "take_distinct() beyond the pool");
        for i in 0..count {
            let j = i + self.range(pool.len() - i);
            pool.swap(i, j);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        assert_ne!(a.next_u64(), b.next_u64());
    }

    #[test]
    fn weighted_respects_a_zero_weight() {
        let mut rng = Rng::new(9);
        for _ in 0..500 {
            assert_ne!(rng.weighted(&[10, 0, 10]), 1);
        }
    }

    #[test]
    fn weighted_reaches_every_nonzero_index() {
        let mut rng = Rng::new(3);
        let mut seen = [false; 3];
        for _ in 0..500 {
            seen[rng.weighted(&[1, 1, 1])] = true;
        }
        assert_eq!(seen, [true, true, true]);
    }

    #[test]
    fn take_distinct_yields_distinct_indices() {
        let mut rng = Rng::new(11);
        let mut pool: Vec<usize> = (0..20).collect();
        rng.take_distinct(&mut pool, 7);
        let mut head = pool[..7].to_vec();
        head.sort_unstable();
        head.dedup();
        assert_eq!(head.len(), 7);
    }

    #[test]
    fn between_stays_in_range() {
        let mut rng = Rng::new(5);
        for _ in 0..200 {
            let v = rng.between(3, 9);
            assert!((3..=9).contains(&v));
        }
    }
}
