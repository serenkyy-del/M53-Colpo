//! Bounded native catalog picker. Clinical snapshots and writes belong to NativeForm.
use gpui_kit::base::TestSupportExt;
use gpui_kit::component::{
    ActiveTheme, IndexPath,
    combobox::ComboboxState,
    h_flex,
    searchable_list::{SearchableListDelegate, SearchableListItem},
    tooltip::Tooltip,
};
use gpui_kit::{prelude::*, *};
use m53_client::diagnoses::{Catalog, MAX_ENTRIES};
use std::sync::Arc;
use uuid::Uuid;

/// Cloning a visible item shares the catalog rather than copying source strings.
#[derive(Clone)]
pub struct Item {
    catalog: Arc<Catalog>,
    index: usize,
}

impl Item {
    pub fn entry(&self) -> &m53_contracts::diagnoses::Entry {
        &self.catalog.items[self.index]
    }
}

impl SearchableListItem for Item {
    type Value = Uuid;

    fn title(&self) -> SharedString {
        let entry = self.entry();
        format!("{} {}", entry.code, entry.name).into()
    }

    fn value(&self) -> &Uuid {
        &self.entry().guid
    }

    fn matches(&self, query: &str) -> bool {
        if self.disabled() {
            return false;
        }
        let entry = self.entry();
        let fields = [
            entry.code.to_lowercase(),
            entry.name.to_lowercase(),
            entry.full_name.to_lowercase(),
        ];
        query
            .split_whitespace()
            .map(str::to_lowercase)
            .all(|token| fields.iter().any(|field| field.contains(&token)))
    }

    fn disabled(&self) -> bool {
        let entry = self.entry();
        !entry.active || entry.code.trim().is_empty() || entry.name.trim().is_empty()
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let entry = self.entry();
        let tooltip = if entry.full_name.trim().is_empty() {
            format!("{} {}", entry.code, entry.name)
        } else {
            format!("{} {}\n{}", entry.code, entry.name, entry.full_name)
        };
        h_flex()
            .id(("diagnosis-option", self.index))
            .test_support()
            .w_full()
            .min_w_0()
            .gap_2()
            .child(
                div()
                    .w(px(64.))
                    .flex_shrink_0()
                    .truncate()
                    .child(entry.code.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(entry.name.clone()),
            )
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
    }
}

/// Only the at-most-100 visible Arc/index pairs are rebuilt during search.
pub struct Delegate {
    catalog: Arc<Catalog>,
    priorities: Arc<Vec<Uuid>>,
    visible: [Vec<Item>; 2],
}

impl Delegate {
    fn new(catalog: Arc<Catalog>, selected: Option<Uuid>, priorities: Arc<Vec<Uuid>>) -> Self {
        let mut indices = catalog.search_prioritized("", true, MAX_ENTRIES, &priorities);
        // Retain an existing source selection even if outside the first page or
        // inactive. It is disabled for new choices; the caller owns saved labels.
        if let Some(index) = selected.and_then(|guid| {
            catalog
                .items
                .binary_search_by_key(&guid, |entry| entry.guid)
                .ok()
        }) {
            if !indices.contains(&index) {
                if indices.len() == MAX_ENTRIES {
                    indices.pop();
                }
                indices.push(index);
            }
        }
        let mut result = Self {
            catalog,
            priorities,
            visible: [vec![], vec![]],
        };
        result.set_indices(indices);
        result
    }
    fn set_indices(&mut self, indices: Vec<usize>) {
        self.visible = [vec![], vec![]];
        let preferred: std::collections::HashSet<Uuid> = self.priorities.iter().copied().collect();
        for index in indices {
            let section = usize::from(!preferred.contains(&self.catalog.items[index].guid));
            self.visible[section].push(Item {
                catalog: self.catalog.clone(),
                index,
            });
        }
    }
    fn rebuild(&mut self, query: &str) {
        let indices = self
            .catalog
            .search_prioritized(query, true, MAX_ENTRIES, &self.priorities);
        self.set_indices(indices);
    }
}

impl SearchableListDelegate for Delegate {
    type Item = Item;

    fn sections_count(&self, _: &App) -> usize {
        2
    }

    fn render_section_header(
        &self,
        section: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        Some(
            div()
                .px_3()
                .py_1()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child(if section == 0 {
                    "Основные диагнозы"
                } else {
                    "Другие диагнозы"
                })
                .into_any_element(),
        )
    }

    fn items_count(&self, section: usize) -> usize {
        self.visible.get(section).map_or(0, Vec::len)
    }

    fn item(&self, index: IndexPath) -> Option<&Item> {
        self.visible
            .get(index.section)
            .and_then(|items| items.get(index.row))
    }

    fn position<V>(&self, value: &V) -> Option<IndexPath>
    where
        Self::Item: SearchableListItem<Value = V>,
        V: PartialEq,
    {
        self.visible
            .iter()
            .enumerate()
            .find_map(|(section, items)| {
                items
                    .iter()
                    .position(|item| item.value() == value)
                    .map(|row| IndexPath::default().section(section).row(row))
            })
    }

    fn perform_search(&mut self, query: &str, _: &mut Window, _: &mut App) -> Task<()> {
        self.rebuild(query);
        Task::ready(())
    }
}

pub type PickerState = ComboboxState<Delegate>;

#[cfg(test)]
mod tests {
    use super::{
        Arc, Catalog, Delegate, IndexPath, SearchableListDelegate, SearchableListItem, Uuid,
    };
    #[test]
    fn picker_groups_priorities_before_others_and_keeps_selection_identity() {
        let first = Uuid::from_u128(1);
        let preferred = Uuid::from_u128(2);
        let items = [first, preferred]
            .into_iter()
            .map(|guid| m53_contracts::diagnoses::Entry {
                guid,
                code: format!("N{}", guid.as_u128()),
                name: "Запись".into(),
                full_name: String::new(),
                active: true,
                is_folder: false,
                source_version: "1".into(),
            })
            .collect();
        let catalog = Arc::new(Catalog {
            metadata: m53_contracts::diagnoses::Metadata {
                version_id: Uuid::from_u128(3),
                source_id: Uuid::from_u128(4),
                synced_at: String::new(),
                count: 2,
            },
            items,
        });
        let mut delegate = Delegate::new(catalog, Some(first), Arc::new(vec![preferred]));
        assert_eq!(delegate.items_count(0), 1);
        assert_eq!(delegate.items_count(1), 1);
        assert_eq!(
            *delegate
                .item(IndexPath::default().section(0).row(0))
                .unwrap()
                .value(),
            preferred
        );
        assert_eq!(
            *delegate
                .item(IndexPath::default().section(1).row(0))
                .unwrap()
                .value(),
            first
        );
        delegate.rebuild("N1");
        assert_eq!(delegate.items_count(0), 0);
        assert_eq!(delegate.items_count(1), 1);
    }
}

/// No implicit first choice. A missing saved GUID yields no picker selection;
/// this is never an instruction to clear the caller's persisted snapshot.
pub fn new(
    catalog: Arc<Catalog>,
    selected: Option<Uuid>,
    priorities: Arc<Vec<Uuid>>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PickerState> {
    let delegate = Delegate::new(catalog, selected, priorities);
    let selected_indices = selected
        .and_then(|guid| delegate.position(&guid))
        .into_iter()
        .collect();
    cx.new(|cx| ComboboxState::new(delegate, selected_indices, window, cx).searchable(true))
}
