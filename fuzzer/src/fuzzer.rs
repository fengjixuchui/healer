use crate::corpus::Corpus;
use crate::exec::Executor;
use crate::feedback::{Block, Branch, FeedBack};
use crate::guest::Crash;
use crate::report::TestCaseRecord;
use core::analyze::prog_analyze;
use core::analyze::RTable;
use core::c::to_script;
use core::gen::gen;
use core::minimize::remove;
use core::prog::Prog;
use core::target::Target;
use executor::{ExecResult, Reason};
use fots::types::GroupId;
use itertools::Itertools;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

pub struct Fuzzer {
    pub target: Arc<Target>,
    pub rt: Arc<RwLock<HashMap<GroupId, RTable>>>,
    pub conf: core::gen::Config,
    pub corpus: Arc<Corpus>,
    pub feedback: Arc<FeedBack>,
    pub record: Arc<TestCaseRecord>,
}

impl Fuzzer {
    pub fn fuzz(self, mut executor: Executor, corpus: Vec<Prog>) {
        for p in corpus.into_iter() {
            self.exec_one(p, &mut executor);
        }

        loop {
            let p = {
                let rt = self.rt.read().unwrap();
                gen(&self.target, &rt, &self.conf)
            };
            self.exec_one(p, &mut executor);
        }
    }

    fn exec_one(&self, p: Prog, executor: &mut Executor) {
        match executor.exec(&p) {
            Ok(exec_result) => match exec_result {
                ExecResult::Ok(raw_branches) => self.feedback_analyze(p, raw_branches, executor),
                ExecResult::Failed(reason) => self.failed_analyze(p, reason),
            },
            Err(crash) => self.crash_analyze(p, crash, executor),
        }
    }

    fn failed_analyze(&self, p: Prog, reason: Reason) {
        self.record.insert_failed(p, reason)
    }

    fn crash_analyze(&self, p: Prog, crash: Crash, executor: &mut Executor) {
        warn!("========== Crashed ========= \n{}", crash);
        if !crash.inner.contains("CRASH-MEMLEAK") {
            let stmts = to_script(&p, &self.target);
            warn!("Caused by:\n{}", stmts.to_string());
            warn!("Restarting to repro ...");
            executor.start();
            match executor.exec(&p) {
                Ok(exec_result) => {
                    match exec_result {
                        ExecResult::Ok(_) => warn!("Repo failed, executed successfully"),
                        ExecResult::Failed(reason) => {
                            warn!("Repo failed, executed failed: {}", reason)
                        }
                    };
                    self.record.insert_crash(p, crash, false)
                }
                Err(repo_crash) => {
                    self.record.insert_crash(p, repo_crash, true);
                    warn!("Repo successfully, restarting guest ...");
                    executor.start();
                }
            }
        }
    }

    fn feedback_analyze(&self, p: Prog, raw_blocks: Vec<Vec<usize>>, executor: &mut Executor) {
        for (call_index, raw_blocks) in raw_blocks.iter().enumerate() {
            let (new_blocks_1, new_branches_1) = self.check_new_feedback(raw_blocks);

            if !new_blocks_1.is_empty() || !new_branches_1.is_empty() {
                let p = p.sub_prog(call_index);
                let exec_result = self.exec_no_crash(executor, &p);

                if let ExecResult::Ok(raw_blocks) = exec_result {
                    if raw_blocks.len() == call_index + 1 {
                        let (new_block_2, new_branches_2) =
                            self.check_new_feedback(&raw_blocks[call_index]);

                        let new_block: HashSet<_> =
                            new_blocks_1.intersection(&new_block_2).cloned().collect();
                        let new_branches: HashSet<_> = new_branches_1
                            .intersection(&new_branches_2)
                            .cloned()
                            .collect();

                        if !new_block.is_empty() || !new_branches.is_empty() {
                            let minimized_p = self.minimize(&p, &new_block, executor);
                            let raw_branches = self.exec_no_fail(executor, &minimized_p);
                            {
                                let g = &self.target.groups[&p.gid];
                                let mut r = self.rt.write().unwrap();
                                prog_analyze(g, r.get_mut(&p.gid).unwrap(), &p);
                            }

                            let mut blocks = Vec::new();
                            let mut branches = Vec::new();
                            for raw_branches in raw_branches.iter() {
                                let (block, branch) = self.cook_raw_block(raw_branches);
                                blocks.push(block);
                                branches.push(branch);
                            }

                            blocks.shrink_to_fit();
                            branches.shrink_to_fit();

                            self.record.insert_executed(
                                &minimized_p,
                                &blocks[..],
                                &branches[..],
                                &new_block,
                                &new_branches,
                            );
                            self.corpus.insert(minimized_p);
                            self.feedback.merge(new_block, new_branches);
                        }
                    }
                }
            }
        }
    }

    fn minimize(&self, p: &Prog, new_block: &HashSet<Block>, executor: &mut Executor) -> Prog {
        assert!(!p.calls.is_empty());

        let mut p = p.clone();
        if p.len() == 1 {
            return p;
        }

        let mut p_orig;
        let mut i = 0;
        while i != p.len() - 1 {
            p_orig = p.clone();
            if !remove(&mut p, i) {
                i += 1;
            } else if let ExecResult::Ok(cover) = self.exec_no_crash(executor, &p) {
                let (new_blocks_1, _) = self.check_new_feedback(cover.last().unwrap());
                if new_blocks_1.is_empty() || new_blocks_1.intersection(new_block).count() == 0 {
                    i += 1;
                    p = p_orig;
                }
            } else {
                p = p_orig;
                return p;
            }
        }
        p
    }

    fn check_new_feedback(&self, raw_blocks: &[usize]) -> (HashSet<Block>, HashSet<Branch>) {
        let (blocks, branches) = self.cook_raw_block(raw_blocks);
        let new_blocks = self.feedback.diff_block(&blocks[..]);
        let new_branches = self.feedback.diff_branch(&branches[..]);
        (new_blocks, new_branches)
    }

    /// calculate branch, return depuped blocks and branches
    fn cook_raw_block(&self, raw_blocks: &[usize]) -> (Vec<Block>, Vec<Branch>) {
        let mut blocks: Vec<Block> = raw_blocks.iter().map(|b| Block::from(*b)).collect();
        let mut branches: Vec<Branch> = blocks
            .iter()
            .cloned()
            .tuple_windows()
            .map(|(b1, b2)| Branch::from((b1, b2)))
            .collect();

        blocks.sort();
        blocks.dedup();
        blocks.shrink_to_fit();
        branches.sort();
        branches.dedup();
        branches.shrink_to_fit();
        (blocks, branches)
    }

    fn exec_no_crash(&self, executor: &mut Executor, p: &Prog) -> ExecResult {
        match executor.exec(p) {
            Ok(exec_result) => exec_result,
            Err(crash) => {
                if crash.inner.contains("CRASH-MEMLEAK") {
                    warn!("========== Crashed ========= \n{}", crash);
                    return ExecResult::Failed(Reason(String::from("Mem leak detected")));
                }
                exits!(exitcode::SOFTWARE, "Unexpected crash: {}", crash)
            }
        }
    }

    fn exec_no_fail(&self, executor: &mut Executor, p: &Prog) -> Vec<Vec<usize>> {
        match executor.exec(p) {
            Ok(exec_result) => match exec_result {
                ExecResult::Ok(raw_branches) => raw_branches,
                ExecResult::Failed(_) => Default::default(),
            },
            Err(crash) => {
                if crash.inner.contains("CRASH-MEMLEAK") {
                    warn!("========== Crashed ========= \n{}", crash);
                    return Default::default();
                }

                exits!(exitcode::SOFTWARE, "Unexpected crash: {}", crash)
            }
        }
    }
}
