//! Pure full-catalog staging and offline search; no clinical selection or I/O.
use m53_contracts::diagnoses::{Entry, Metadata, Page};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const MAX_ENTRIES: usize = 100_000;
pub const MAX_PAGE: usize = 500;
pub const MAX_RESULTS: usize = 100;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub metadata: Metadata,
    pub items: Vec<Entry>,
}

impl Catalog {
    /// Validate persisted data before publishing it to a profile's UI.
    pub fn validate(&self) -> Result<(), String> {
        validate_metadata(&self.metadata)?;
        if self.items.len() as u64 != self.metadata.count {
            return Err("Diagnosis catalog count mismatch".into());
        }
        validate_items(&self.items, None)
    }
    /// Indices refer to the unchanged source catalog, never to a filtered position.
    /// Querying does not select a diagnosis or modify any clinical snapshot.
    pub fn search(&self, query: &str, include_groups: bool, limit: usize) -> Vec<usize> {
        self.search_prioritized(query, include_groups, limit.min(MAX_RESULTS), &[])
    }
    /// Reorder only the result indices; the GUID-sorted source stays untouched.
    pub fn search_prioritized(
        &self,
        query: &str,
        include_groups: bool,
        limit: usize,
        priorities: &[Uuid],
    ) -> Vec<usize> {
        let tokens: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let mut ranks = std::collections::HashMap::new();
        for (rank, guid) in priorities.iter().enumerate() {
            ranks.entry(*guid).or_insert(rank);
        }
        let mut results: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                if !item.active
                    || (!include_groups && item.is_folder)
                    || item.code.trim().is_empty()
                    || item.name.trim().is_empty()
                {
                    return false;
                }
                let fields = [
                    item.code.to_lowercase(),
                    item.name.to_lowercase(),
                    item.full_name.to_lowercase(),
                ];
                tokens
                    .iter()
                    .all(|token| fields.iter().any(|field| field.contains(token)))
            })
            .map(|(index, _)| index)
            .take(if ranks.is_empty() {
                limit.min(MAX_ENTRIES)
            } else {
                usize::MAX
            })
            .collect();
        results.sort_by_key(|index| {
            ranks
                .get(&self.items[*index].guid)
                .copied()
                .unwrap_or(usize::MAX)
        });
        results.truncate(limit.min(MAX_ENTRIES));
        results
    }
}

fn validate_metadata(metadata: &Metadata) -> Result<(), String> {
    if metadata.version_id.is_nil() || metadata.source_id.is_nil() {
        return Err("Diagnosis catalog identity is missing".into());
    }
    if metadata.count > MAX_ENTRIES as u64 {
        return Err("Diagnosis catalog exceeds entry limit".into());
    }
    Ok(())
}

fn validate_items(items: &[Entry], mut previous: Option<Uuid>) -> Result<(), String> {
    for item in items {
        if item.guid.is_nil() || previous.is_some_and(|id| item.guid <= id) {
            return Err("Diagnosis catalog GUIDs are not strictly increasing".into());
        }
        if item.code.len() > 32
            || item.name.len() > 4096
            || item.full_name.len() > 16_384
            || item.source_version.len() > 256
        {
            return Err("Diagnosis catalog entry exceeds text limit".into());
        }
        previous = Some(item.guid);
    }
    Ok(())
}

pub struct Download {
    metadata: Metadata,
    items: Vec<Entry>,
    after: Option<Uuid>,
    closed: bool,
}
impl Download {
    pub fn new(metadata: Metadata) -> Result<Self, String> {
        validate_metadata(&metadata)?;
        Ok(Self {
            metadata,
            items: vec![],
            after: None,
            closed: false,
        })
    }
    pub fn next_after(&self) -> Option<Uuid> {
        self.after
    }
    /// A failed stage is consumed and cannot later publish even a valid-looking tail.
    pub fn push(&mut self, page: Page) -> Result<Option<Catalog>, String> {
        if self.closed {
            return Err("Diagnosis catalog download is closed".into());
        }
        let result = self.accept(page);
        if result.is_err() {
            self.closed = true;
            self.items.clear();
            self.after = None;
        }
        result
    }
    fn accept(&mut self, page: Page) -> Result<Option<Catalog>, String> {
        if page.version_id != self.metadata.version_id {
            return Err("Diagnosis catalog version changed during download".into());
        }
        if page.items.len() > MAX_PAGE {
            return Err("Diagnosis catalog page exceeds entry limit".into());
        }
        validate_items(&page.items, self.after)?;
        let count = self.items.len() + page.items.len();
        if count as u64 > self.metadata.count || count > MAX_ENTRIES {
            return Err("Diagnosis catalog has more entries than advertised".into());
        }
        if let Some(next) = page.next_after {
            if page.items.last().map(|item| item.guid) != Some(next)
                || self.after.is_some_and(|previous| next <= previous)
                || count as u64 >= self.metadata.count
            {
                return Err("Diagnosis catalog page cursor does not progress".into());
            }
            self.items.extend(page.items);
            self.after = Some(next);
            return Ok(None);
        }
        if count as u64 != self.metadata.count {
            return Err("Diagnosis catalog completed before advertised count".into());
        }
        self.items.extend(page.items);
        self.closed = true;
        let catalog = Catalog {
            metadata: self.metadata.clone(),
            items: std::mem::take(&mut self.items),
        };
        catalog.validate()?;
        Ok(Some(catalog))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata(count: u64) -> Metadata {
        Metadata {
            version_id: Uuid::from_u128(100),
            source_id: Uuid::from_u128(200),
            synced_at: "2026-10-26T00:00:00Z".into(),
            count,
        }
    }
    fn entry(id: u128) -> Entry {
        Entry {
            guid: Uuid::from_u128(id),
            code: format!("N{id}"),
            name: "Воспаление шейки".into(),
            full_name: "Хроническое воспаление шейки матки".into(),
            active: true,
            is_folder: false,
            source_version: "v1".into(),
        }
    }
    fn page(items: Vec<Entry>, next: Option<u128>) -> Page {
        Page {
            version_id: metadata(0).version_id,
            items,
            next_after: next.map(Uuid::from_u128),
        }
    }
    #[test]
    fn diagnoses_complete_only_after_final_page_and_preserve_source_codes() {
        let mut download = Download::new(metadata(2)).unwrap();
        let mut first = entry(1);
        first.code = " 001.0 ".into();
        assert!(
            download
                .push(page(vec![first.clone()], Some(1)))
                .unwrap()
                .is_none()
        );
        assert_eq!(download.next_after(), Some(first.guid));
        let catalog = download.push(page(vec![entry(2)], None)).unwrap().unwrap();
        assert_eq!(catalog.items[0].code, " 001.0 ");
        assert_eq!(catalog.metadata.count, 2);
        assert!(catalog.validate().is_ok());
        assert!(download.push(page(vec![], None)).is_err());
    }
    #[test]
    fn diagnoses_valid_empty_catalog_differs_from_absent() {
        let mut download = Download::new(metadata(0)).unwrap();
        let catalog = download
            .push(page(vec![], None))
            .unwrap()
            .expect("complete empty version");
        assert!(catalog.items.is_empty());
        assert!(catalog.validate().is_ok());
    }
    #[test]
    fn diagnoses_metadata_limits_and_nil_identity() {
        let mut nil_source = metadata(1);
        nil_source.source_id = Uuid::nil();
        for meta in [metadata(100_001), nil_source] {
            assert!(Download::new(meta).is_err());
        }
        let mut meta = metadata(0);
        meta.version_id = Uuid::nil();
        assert!(Download::new(meta).is_err());
        assert!(Download::new(metadata(MAX_ENTRIES as u64)).is_ok());
    }
    #[test]
    fn diagnoses_invalid_page_poisoning_never_publishes_partial_catalog() {
        let mut cases = vec![
            page(vec![entry(2), entry(1)], None),
            page(vec![entry(1), entry(1)], None),
            page(vec![entry(0)], None),
            page(vec![entry(1)], Some(2)),
            page(vec![], Some(1)),
            page(vec![entry(1)], None),
            page(vec![entry(1), entry(2)], Some(2)),
        ];
        let mut wrong_version = page(vec![entry(1), entry(2)], None);
        wrong_version.version_id = Uuid::from_u128(101);
        cases.push(wrong_version);
        cases.push(page((1..=501).map(entry).collect(), None));
        for bad in cases {
            let mut download = Download::new(metadata(2)).unwrap();
            assert!(download.push(bad).is_err());
            assert!(
                download.push(page(vec![entry(1), entry(2)], None)).is_err(),
                "failed stage cannot recover/publish"
            );
        }
    }
    #[test]
    fn diagnoses_page_order_must_progress_across_page_boundaries() {
        let mut download = Download::new(metadata(3)).unwrap();
        assert!(
            download
                .push(page(vec![entry(2)], Some(2)))
                .unwrap()
                .is_none()
        );
        assert!(download.push(page(vec![entry(1), entry(3)], None)).is_err());
    }
    #[test]
    fn diagnoses_string_limits_are_utf8_bytes() {
        for field in ["code", "name", "full_name", "source_version"] {
            let limit = match field {
                "code" => 32,
                "name" => 4096,
                "full_name" => 16384,
                _ => 256,
            };
            let mut item = entry(1);
            let target = match field {
                "code" => &mut item.code,
                "name" => &mut item.name,
                "full_name" => &mut item.full_name,
                _ => &mut item.source_version,
            };
            *target = "я".repeat(limit / 2);
            let catalog = Catalog {
                metadata: metadata(1),
                items: vec![item.clone()],
            };
            assert!(catalog.validate().is_ok(), "exact bound {field}");
            match field {
                "code" => &mut item.code,
                "name" => &mut item.name,
                "full_name" => &mut item.full_name,
                _ => &mut item.source_version,
            }
            .push('a');
            let mut download = Download::new(metadata(1)).unwrap();
            assert!(
                download.push(page(vec![item], None)).is_err(),
                "over bound {field}"
            );
        }
    }
    #[test]
    fn diagnoses_cached_catalog_validation_rejects_count_and_order_corruption() {
        assert!(
            Catalog {
                metadata: metadata(2),
                items: vec![entry(1)]
            }
            .validate()
            .is_err()
        );
        assert!(
            Catalog {
                metadata: metadata(2),
                items: vec![entry(2), entry(1)]
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn diagnoses_search_uses_all_tokens_and_stable_indices_without_selection() {
        let mut items = vec![entry(1), entry(2), entry(3), entry(4), entry(5), entry(6)];
        items[1].active = false;
        items[2].code = "  ".into();
        items[3].name = "".into();
        items[4].is_folder = true;
        let catalog = Catalog {
            metadata: metadata(6),
            items,
        };
        assert_eq!(catalog.search("ВОСПАЛЕНИЕ n1", false, 20), vec![0]);
        assert_eq!(catalog.search("матки хроническое", false, 20), vec![0, 5]);
        assert_eq!(catalog.search("матки хроническое", true, 20), vec![0, 4, 5]);
        assert_eq!(
            catalog.search("несуществующий", true, 20),
            Vec::<usize>::new()
        );
        assert_eq!(catalog.search("", true, 1), vec![0]);
        assert!(catalog.search("", true, 0).is_empty());
        assert_eq!(catalog.items[0].code, "N1");
    }
    #[test]
    fn diagnoses_priorities_are_exact_ordered_and_applied_before_limit() {
        let mut items: Vec<_> = (1..=110).map(entry).collect();
        for (i, item) in items.iter_mut().enumerate() {
            item.code = format!("X{i}");
        }
        items[105].code = "N87.0".into();
        items[106].code = "N72".into();
        items[107].code = "N870".into();
        items[108].code = "N87".into();
        items[109].code = "N88.0".into();
        items[109].active = false;
        let catalog = Catalog {
            metadata: metadata(110),
            items,
        };
        let priorities: Vec<Uuid> = [107, 109, 106, 110]
            .into_iter()
            .map(Uuid::from_u128)
            .collect();
        let indices = catalog.search_prioritized("", true, 100, &priorities);
        assert_eq!(&indices[..3], &[106, 108, 105]);
        assert!(!indices.contains(&109));
        assert!(!indices.contains(&107));
        assert_eq!(catalog.items[105].code, "N87.0");
        assert_eq!(
            catalog.search_prioritized("n87", true, 100, &priorities)[..2],
            [108, 105]
        );
        assert_eq!(catalog.search_prioritized("", true, 3, &[]), vec![0, 1, 2]);
    }
    #[test]
    fn priority_picker_can_show_all_results_while_regular_search_stays_bounded() {
        let catalog = Catalog {
            metadata: metadata(1000),
            items: (1..=1000).map(entry).collect(),
        };
        assert_eq!(
            catalog
                .search_prioritized("", true, MAX_ENTRIES, &[Uuid::from_u128(1000)])
                .len(),
            1000
        );
        assert_eq!(catalog.search("", true, MAX_ENTRIES).len(), MAX_RESULTS);
    }
    #[test]
    fn diagnoses_search_results_have_hard_bound() {
        let catalog = Catalog {
            metadata: metadata(1000),
            items: (1..=1000).map(entry).collect(),
        };
        assert_eq!(catalog.search("", true, usize::MAX).len(), MAX_RESULTS);
    }
}
