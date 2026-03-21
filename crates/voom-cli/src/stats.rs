//! Data aggregation utilities used by `report`, `status`, and other commands.

use voom_domain::media::MediaFile;

/// Count occurrences of string keys extracted from items, sorted by frequency (descending).
///
/// The `key_fn` closure is called once per item and should return an iterator of
/// string keys to tally (one or more per item).
pub fn count_by<T, I, F>(items: &[T], key_fn: F) -> Vec<(String, usize)>
where
    F: Fn(&T) -> I,
    I: IntoIterator<Item = String>,
{
    let mut counts = std::collections::HashMap::new();
    for item in items {
        for key in key_fn(item) {
            *counts.entry(key).or_insert(0usize) += 1;
        }
    }
    let mut sorted: Vec<_> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    sorted
}

/// Count occurrences of each container format, sorted by frequency (descending).
pub fn container_counts(files: &[MediaFile]) -> Vec<(String, usize)> {
    count_by(files, |f| std::iter::once(f.container.as_str().to_string()))
}
