use crate::feedback::{Block, Branch};
use crate::guest::Crash;
use crate::mail;
use chrono::prelude::*;
use chrono::DateTime;
use circular_queue::CircularQueue;
use core::c::to_script;
use core::prog::Prog;
use core::target::Target;
use executor::Reason;
use lettre_email::EmailBuilder;
use serde::Serialize;
use std::collections::HashSet;
use std::fs::write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

pub struct TestCaseRecord {
    normal: Mutex<CircularQueue<ExecutedCase>>,
    failed: Mutex<CircularQueue<FailedCase>>,
    crash: Mutex<CircularQueue<CrashedCase>>,
    target: Arc<Target>,
    id_n: AtomicUsize,
    normal_num: AtomicUsize,
    failed_num: AtomicUsize,
    crashed_num: AtomicUsize,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct TestCase {
    pub id: usize,
    pub title: String,
    pub test_time: DateTime<Local>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct ExecutedCase {
    pub meta: TestCase,
    /// execute test program
    pub p: String,
    /// number of blocks per call
    pub block_num: Vec<usize>,
    /// number of branchs per call
    pub branch_num: Vec<usize>,
    /// new branch of last call
    pub new_branch: usize,
    /// new block of last call
    pub new_block: usize,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct FailedCase {
    pub meta: TestCase,
    pub p: String,
    pub reason: String,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct CrashedCase {
    pub meta: TestCase,
    pub p: String,
    pub repo: bool,
    pub crash: Crash,
}

#[allow(clippy::len_without_is_empty)]
impl TestCaseRecord {
    pub fn new(t: Arc<Target>) -> Self {
        Self {
            normal: Mutex::new(CircularQueue::with_capacity(1024 * 64)),
            failed: Mutex::new(CircularQueue::with_capacity(1024 * 64)),
            crash: Mutex::new(CircularQueue::with_capacity(1024)),
            target: t,
            id_n: AtomicUsize::new(0),
            normal_num: AtomicUsize::new(0),
            failed_num: AtomicUsize::new(0),
            crashed_num: AtomicUsize::new(0),
        }
    }

    pub fn insert_executed(
        &self,
        p: &Prog,
        blocks: &[Vec<Block>],
        branches: &[Vec<Branch>],
        new_block: &HashSet<Block>,
        new_branch: &HashSet<Branch>,
    ) {
        let block_num = blocks.iter().map(|blocks| blocks.len()).collect();
        let branch_num = branches.iter().map(|branches| branches.len()).collect();
        let id = self.next_id();
        let title = self.title_of(&p, id);
        let stmts = to_script(&p, &self.target);
        let case = ExecutedCase {
            meta: TestCase {
                id,
                title,
                test_time: Local::now(),
            },
            p: stmts.to_string(),
            block_num,
            branch_num,
            new_branch: new_branch.len(),
            new_block: new_block.len(),
        };
        {
            let mut execs = self.normal.lock().unwrap();
            execs.push(case);
        }
        self.normal_num.fetch_add(1, Ordering::SeqCst);
    }

    pub fn insert_crash(&self, p: Prog, crash: Crash, repo: bool) {
        let id = self.next_id();
        let stmts = to_script(&p, &self.target);
        let case = CrashedCase {
            meta: TestCase {
                id,
                title: self.title_of(&p, id),
                test_time: Local::now(),
            },
            p: stmts.to_string(),
            crash,
            repo,
        };

        self.persist_crash_case(&case);

        {
            let mut crashes = self.crash.lock().unwrap();
            crashes.push(case);
        }
        self.crashed_num.fetch_add(1, Ordering::SeqCst);
        // {
        //     let mut crashed_num = self.crashed_num.lock().unwrap();
        //     *crashed_num += 1;
        // }
    }

    pub fn insert_failed(&self, p: Prog, reason: Reason) {
        let id = self.next_id();
        let stmts = to_script(&p, &self.target);
        let case = FailedCase {
            meta: TestCase {
                id,
                title: self.title_of(&p, id),
                test_time: Local::now(),
            },
            p: stmts.to_string(),
            reason: reason.to_string(),
        };
        {
            let mut failed_cases = self.failed.lock().unwrap();
            failed_cases.push(case);
        }
        self.failed_num.fetch_add(1, Ordering::SeqCst);
        // {
        //     let mut failed_num = self.failed_num.lock().unwrap();
        //     *failed_num += 1;
        // }
    }

    pub fn psersist(&self) {
        self.persist_normal_case();
        self.persist_failed_case();
    }

    pub fn len(&self) -> (usize, usize, usize) {
        (
            self.normal_num.load(Ordering::SeqCst),
            self.failed_num.load(Ordering::SeqCst),
            self.crashed_num.load(Ordering::SeqCst),
        )
    }

    fn persist_normal_case(&self) {
        let cases = {
            let cases = self.normal.lock().unwrap();
            if cases.is_empty() {
                return;
            }
            cases.asc_iter().cloned().collect::<Vec<_>>()
        };

        let path = "./normal_case.json".to_string();
        let report = serde_json::to_string_pretty(&cases).unwrap();

        write(&path, report).unwrap_or_else(|e| {
            exits!(
                exitcode::IOERR,
                "Fail to persist normal test case to {} : {}",
                path,
                e
            )
        })
    }

    fn persist_failed_case(&self) {
        let cases = {
            let cases = self.failed.lock().unwrap();
            if cases.is_empty() {
                return;
            }
            cases.asc_iter().cloned().collect::<Vec<_>>()
        };

        let path = "./failed_case.json".to_string();
        let report = serde_json::to_string_pretty(&cases).unwrap();
        write(&path, report).unwrap_or_else(|e| {
            exits!(
                exitcode::IOERR,
                "Fail to persist failed test case to {} : {}",
                path,
                e
            )
        })
    }

    fn persist_crash_case(&self, case: &CrashedCase) {
        let path = format!("./crashes/{}", &case.meta.title);
        let crash = serde_json::to_string_pretty(case).unwrap();
        let crash_mail = EmailBuilder::new()
            .subject("Healer-Reporter: CRASH REPORT")
            .body(&crash);
        mail::send(crash_mail);
        write(&path, crash).unwrap_or_else(|e| {
            exits!(
                exitcode::IOERR,
                "Fail to persist failed test case to {} : {}",
                path,
                e
            )
        })
    }

    fn title_of(&self, p: &Prog, id: usize) -> String {
        let group = String::from(self.target.group_name_of(p.gid));
        let f = String::from(&self.target.fn_of(p.calls.last().unwrap().fid).dec_name);
        format!("{}_{}_{}", group, f, id)
    }

    fn next_id(&self) -> usize {
        self.id_n.fetch_add(1, Ordering::SeqCst)
    }
}
