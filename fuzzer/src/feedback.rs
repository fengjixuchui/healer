use std::collections::HashSet;
use std::iter::Extend;
use std::sync::RwLock;

#[derive(Clone, Debug, Default, Hash, PartialOrd, PartialEq, Ord, Eq)]
pub struct Block(usize);

impl From<usize> for Block {
    fn from(raw: usize) -> Self {
        Self(raw)
    }
}

#[derive(Clone, Debug, Default, Hash, PartialOrd, PartialEq, Ord, Eq)]
pub struct Branch(usize);

impl From<(Block, Block)> for Branch {
    fn from((b1, b2): (Block, Block)) -> Self {
        let mut a = b1.0;
        // hash algorithm from syzkaller
        a = (a ^ 61) ^ (a >> 16);
        a = a + (a << 3);
        a = a ^ (a >> 4);
        a *= 0x27d4_eb2d;
        a = a ^ (a >> 15);
        Self(a ^ b2.0)
    }
}

#[derive(Default)]
pub struct FeedBack {
    branches: RwLock<HashSet<Branch>>,
    blocks: RwLock<HashSet<Block>>,
}

impl FeedBack {
    pub fn diff_branch(&self, branches: &[Branch]) -> HashSet<Branch> {
        let inner = self.branches.read().unwrap();

        let mut result = HashSet::new();
        for b in branches {
            if !inner.contains(b) {
                result.insert(b.clone());
            }
        }
        result.shrink_to_fit();
        result
    }

    pub fn diff_block(&self, blocks: &[Block]) -> HashSet<Block> {
        let inner = self.blocks.read().unwrap();

        let mut result = HashSet::new();
        for b in blocks {
            if !inner.contains(b) {
                result.insert(b.clone());
            }
        }
        result.shrink_to_fit();
        result
    }

    pub fn merge(&self, blocks: HashSet<Block>, branches: HashSet<Branch>) {
        {
            let mut inner = self.branches.write().unwrap();
            inner.extend(branches);
        }
        {
            let mut inner = self.blocks.write().unwrap();
            inner.extend(blocks);
        }
    }

    pub fn is_empty(&self) -> bool {
        let block_empty = {
            let inner = self.blocks.read().unwrap();
            inner.is_empty()
        };
        let branch_empty = {
            let inner = self.branches.read().unwrap();
            inner.is_empty()
        };
        block_empty || branch_empty
    }

    pub fn len(&self) -> (usize, usize) {
        (
            {
                let inner = self.blocks.read().unwrap();
                inner.len()
            },
            {
                let inner = self.branches.read().unwrap();
                inner.len()
            },
        )
    }
}
