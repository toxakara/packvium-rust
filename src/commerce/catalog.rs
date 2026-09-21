//! Versioned item, carton and pallet master data.
//!
//! A packing decision is only as trustworthy as the master data it was made against, so
//! every version is immutable, numbered and effective-dated, a rollback is itself a new
//! higher-numbered version, and an unresolvable reference is a named rejection rather
//! than a silent wrong answer.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct ItemMaster {
    pub id: String,
    pub dimensions_mm: [i64; 3],
    pub weight_g: i64,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct CartonMaster {
    pub id: String,
    pub inner_dimensions_mm: [i64; 3],
    pub max_payload_g: i64,
    pub cost_minor: i64,
}

#[derive(Debug, Clone)]
pub struct PalletMaster {
    pub id: String,
    pub deck_dimensions_mm: [i64; 2],
    pub max_payload_g: i64,
    pub max_stack_height_mm: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExclusionScope {
    ItemCarton,
    ItemPallet,
}

impl ExclusionScope {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "item_carton" => Some(Self::ItemCarton),
            "item_pallet" => Some(Self::ItemPallet),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExclusionRule {
    pub id: String,
    pub scope: ExclusionScope,
    pub subject_id: String,
    pub excluded_id: String,
    pub reason: String,
}

/// A facility-specific override of a base master-data entry.
#[derive(Debug, Clone)]
pub enum OverrideEntry {
    Item(ItemMaster),
    Carton(CartonMaster),
    Pallet(PalletMaster),
}

impl OverrideEntry {
    pub fn id(&self) -> &str {
        match self {
            Self::Item(entry) => &entry.id,
            Self::Carton(entry) => &entry.id,
            Self::Pallet(entry) => &entry.id,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FacilityOverride {
    pub id: String,
    pub facility_id: String,
    pub entry_id: String,
    pub entry: OverrideEntry,
}

/// The complete, immutable content of one catalog version.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub items: Vec<ItemMaster>,
    pub cartons: Vec<CartonMaster>,
    pub pallets: Vec<PalletMaster>,
    pub exclusions: Vec<ExclusionRule>,
    pub overrides: Vec<FacilityOverride>,
}

impl Snapshot {
    /// Ids sorted ascending by code point, so no map or set ordering can leak out.
    pub fn item_ids(&self) -> Vec<String> {
        sorted(self.items.iter().map(|entry| entry.id.clone()))
    }

    pub fn carton_ids(&self) -> Vec<String> {
        sorted(self.cartons.iter().map(|entry| entry.id.clone()))
    }

    pub fn pallet_ids(&self) -> Vec<String> {
        sorted(self.pallets.iter().map(|entry| entry.id.clone()))
    }
}

fn sorted(ids: impl Iterator<Item = String>) -> Vec<String> {
    let mut collected: Vec<String> = ids.collect();
    collected.sort();
    collected
}

/// One immutable, numbered entry in a catalog's append-only publication history.
#[derive(Debug, Clone)]
pub struct Version {
    pub number: i64,
    pub snapshot: Snapshot,
    pub effective_at: i64,
    pub published_at: i64,
    pub rolled_back_from: Option<i64>,
    pub note: String,
}

/// A rejection the catalog model is entitled to make.
#[derive(Debug, Clone)]
pub enum CatalogRejection {
    VersionNotFound,
    NoEffectiveVersion,
    AmbiguousReference,
}

/// Per-catalog append-only publication history.
///
/// Complexity: `publish` is `O(1)`, `resolve` is one `O(h)` scan of the history.
#[derive(Debug)]
pub struct CatalogRegistry {
    pub catalog_id: String,
    versions: Vec<Version>,
}

impl CatalogRegistry {
    pub fn new(catalog_id: String) -> Self {
        Self {
            catalog_id,
            versions: Vec::new(),
        }
    }

    pub fn versions(&self) -> &[Version] {
        &self.versions
    }

    pub fn publish(
        &mut self,
        snapshot: Snapshot,
        effective_at: i64,
        published_at: i64,
        note: String,
    ) {
        let number = self.versions.len() as i64 + 1;
        self.versions.push(Version {
            number,
            snapshot,
            effective_at,
            published_at,
            rolled_back_from: None,
            note,
        });
    }

    /// Publish a new version whose snapshot equals a prior version's. History is
    /// append-only: the target and everything after it stay untouched.
    pub fn rollback(
        &mut self,
        to_version: i64,
        published_at: i64,
        effective_at: Option<i64>,
        note: String,
    ) -> Result<(), CatalogRejection> {
        let target = self.version(to_version)?.clone();
        let number = self.versions.len() as i64 + 1;
        self.versions.push(Version {
            number,
            snapshot: target.snapshot,
            effective_at: effective_at.unwrap_or(published_at),
            published_at,
            rolled_back_from: Some(to_version),
            note: if note.is_empty() {
                format!("rollback to version {to_version}")
            } else {
                note
            },
        });
        Ok(())
    }

    pub fn version(&self, number: i64) -> Result<&Version, CatalogRejection> {
        usize::try_from(number)
            .ok()
            .and_then(|number| number.checked_sub(1))
            .and_then(|index| self.versions.get(index))
            .ok_or(CatalogRejection::VersionNotFound)
    }

    /// Resolve one concrete version, pinned by number or by the version effective as of
    /// a given time. Neither, with more than one version published, is undefined.
    pub fn resolve(
        &self,
        version: Option<i64>,
        as_of: Option<i64>,
    ) -> Result<&Version, CatalogRejection> {
        if let Some(number) = version {
            return self.version(number);
        }
        let Some(as_of) = as_of else {
            if self.versions.len() > 1 {
                return Err(CatalogRejection::AmbiguousReference);
            }
            return self
                .versions
                .first()
                .ok_or(CatalogRejection::VersionNotFound);
        };
        // Ties in effective_at break on the higher (later-published) number, so a
        // same-instant correction or rollback wins deterministically.
        self.versions
            .iter()
            .filter(|version| version.effective_at <= as_of)
            .max_by_key(|version| (version.effective_at, version.number))
            .ok_or(CatalogRejection::NoEffectiveVersion)
    }
}

/// The three histories one request is answered against.
#[derive(Debug, Default)]
pub struct Catalogs {
    inner: BTreeMap<String, CatalogRegistry>,
}

impl Catalogs {
    pub fn insert(&mut self, registry: CatalogRegistry) {
        self.inner.insert(registry.catalog_id.clone(), registry);
    }

    pub fn contains(&self, catalog_id: &str) -> bool {
        self.inner.contains_key(catalog_id)
    }

    pub fn get(&self, catalog_id: &str) -> Option<&CatalogRegistry> {
        self.inner.get(catalog_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_history_preserves_boundaries_and_rollback() {
        let mut registry = CatalogRegistry::new("lookup".into());
        for number in 1..=64 {
            registry.publish(Snapshot::default(), number % 7, number, String::new());
        }
        for number in [1, 2, 32, 64] {
            assert_eq!(registry.version(number).unwrap().number, number);
        }
        for number in [i64::MIN, -10, 0, 65, i64::MAX] {
            assert!(matches!(
                registry.version(number),
                Err(CatalogRejection::VersionNotFound)
            ));
        }
        registry.rollback(1, 100, Some(6), String::new()).unwrap();
        assert_eq!(registry.version(65).unwrap().rolled_back_from, Some(1));
        assert_eq!(registry.resolve(None, Some(6)).unwrap().number, 65);
        assert_eq!(registry.resolve(None, Some(0)).unwrap().number, 63);
    }
}
