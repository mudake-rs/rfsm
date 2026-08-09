use std::error::Error;
use std::fmt::{self, Display, Formatter};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot<T> {
    pub value: T,
    pub version: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedStore<T, E> {
    value: T,
    version: u64,
    committed_effects: Vec<E>,
}

impl<T, E> VersionedStore<T, E> {
    pub fn new(value: T) -> Self {
        Self {
            value,
            version: 0,
            committed_effects: Vec::new(),
        }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub const fn version(&self) -> u64 {
        self.version
    }

    pub fn committed_effects(&self) -> &[E] {
        &self.committed_effects
    }

    pub fn begin(&mut self, expected_version: u64) -> Transaction<'_, T, E> {
        Transaction {
            store: self,
            expected_version,
            staged_value: None,
            staged_effects: Vec::new(),
        }
    }
}

impl<T: Clone, E> VersionedStore<T, E> {
    pub async fn load(&self) -> Snapshot<T> {
        Snapshot {
            value: self.value.clone(),
            version: self.version,
        }
    }
}

pub struct Transaction<'a, T, E> {
    store: &'a mut VersionedStore<T, E>,
    expected_version: u64,
    staged_value: Option<T>,
    staged_effects: Vec<E>,
}

impl<T, E> Transaction<'_, T, E> {
    pub async fn store(&mut self, value: T) {
        self.staged_value = Some(value);
    }

    pub async fn push_effect(&mut self, effect: E) {
        self.staged_effects.push(effect);
    }

    pub async fn commit(self) -> Result<(), StoreError> {
        if self.store.version != self.expected_version {
            return Err(StoreError::VersionConflict);
        }
        if let Some(value) = self.staged_value {
            self.store.value = value;
        }
        self.store.committed_effects.extend(self.staged_effects);
        self.store.version += 1;
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreError {
    VersionConflict,
}

impl Display for StoreError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str("durable purchase version changed")
    }
}

impl Error for StoreError {}
