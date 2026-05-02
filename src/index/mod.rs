use std::collections::HashMap;

use rayon::prelude::*;

use crate::error::Result;

pub struct VectorIndex {
    entries: HashMap<String, Vec<f32>>,
}

impl VectorIndex {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    pub fn insert(&mut self, key: String, vector: Vec<f32>) {
        self.entries.insert(key, vector);
    }

    pub fn remove(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// Returns top-k (key, cosine_similarity) pairs, highest score first.
    /// Parallel scan across all indexed vectors using Rayon.
    pub fn search(&self, query: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
        if self.entries.is_empty() || k == 0 {
            return Ok(vec![]);
        }
        let mut scores: Vec<(String, f32)> = self
            .entries
            .par_iter()
            .filter(|(_, v)| v.len() == query.len())
            .map(|(key, vec)| (key.clone(), cosine_similarity(query, vec)))
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scores.truncate(k);
        Ok(scores)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Dimension of indexed vectors, or None if the index is empty.
    pub fn dim(&self) -> Option<usize> {
        self.entries.values().next().map(|v| v.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_search_returns_correct_order() {
        let mut idx = VectorIndex::new();
        idx.insert("a".into(), vec![1.0, 0.0, 0.0]);
        idx.insert("b".into(), vec![0.0, 1.0, 0.0]);
        idx.insert("c".into(), vec![0.9, 0.1, 0.0]);
        let results = idx.search(&[1.0, 0.0, 0.0], 2).unwrap();
        assert_eq!(results[0].0, "a");
        assert_eq!(results[1].0, "c");
    }

    #[test]
    fn remove_excludes_from_results() {
        let mut idx = VectorIndex::new();
        idx.insert("a".into(), vec![1.0, 0.0]);
        idx.remove("a");
        assert!(idx.search(&[1.0, 0.0], 1).unwrap().is_empty());
    }

    #[test]
    fn update_overwrites_old_vector() {
        let mut idx = VectorIndex::new();
        idx.insert("a".into(), vec![1.0, 0.0]);
        idx.insert("a".into(), vec![0.0, 1.0]);
        let results = idx.search(&[0.0, 1.0], 1).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "a");
        assert!((results[0].1 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn dim_reflects_indexed_vectors() {
        let mut idx = VectorIndex::new();
        assert_eq!(idx.dim(), None);
        idx.insert("a".into(), vec![0.1, 0.2, 0.3]);
        assert_eq!(idx.dim(), Some(3));
    }

    #[test]
    fn search_empty_index_returns_empty() {
        let idx = VectorIndex::new();
        assert!(idx.search(&[1.0, 0.0], 5).unwrap().is_empty());
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}
