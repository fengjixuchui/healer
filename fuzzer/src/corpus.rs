use core::prog::Prog;
use std::collections::HashSet;
use std::iter::FromIterator;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub struct Corpus {
    inner: Mutex<HashSet<Prog>>,
}

impl Corpus {
    pub fn insert(&self, p: Prog) -> bool {
        let mut inner = self.inner.lock().unwrap();
        inner.insert(p)
    }

    pub fn len(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.len()
    }

    pub fn is_empty(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.is_empty()
    }

    pub fn dump(&self) -> bincode::Result<Vec<u8>> {
        let mut progs = {
            let inner = self.inner.lock().unwrap();
            inner
                .iter()
                .map(|p| {
                    let mut p = p.clone();
                    p.shrink();
                    p
                })
                .collect::<Vec<_>>()
        };
        progs.shrink_to_fit();
        bincode::serialize(&progs)
    }

    pub fn load(c: &[u8]) -> bincode::Result<Self> {
        let mut progs: Vec<Prog> = bincode::deserialize(c)?;
        progs.shrink_to_fit();
        Ok(Self {
            inner: Mutex::new(HashSet::from_iter(progs)),
        })
    }
}
