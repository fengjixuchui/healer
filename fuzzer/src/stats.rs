use crate::corpus::Corpus;
use crate::feedback::FeedBack;
use crate::mail;
use crate::report::TestCaseRecord;
use circular_queue::CircularQueue;
use lettre_email::EmailBuilder;
use std::fs::write;
use std::process::exit;
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

pub struct StatSource {
    pub corpus: Arc<Corpus>,
    pub feedback: Arc<FeedBack>,
    // pub candidates: Arc<CQueue<Prog>>,
    pub record: Arc<TestCaseRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Stats {
    pub corpus: usize,
    pub blocks: usize,
    pub branches: usize,
    // pub exec:usize,
    // pub gen:usize,
    // pub minimized:usize,
    // pub candidates: usize,
    pub normal_case: usize,
    pub failed_case: usize,
    pub crashed_case: usize,
}

#[derive(Debug, Deserialize, Clone)]
pub struct SamplerConf {
    /// Duration for sampling, per second
    pub sample_interval: u64,
    /// Duration for report, per minites
    pub report_interval: u64,
}

impl SamplerConf {
    pub fn check(&self) {
        if self.sample_interval < 10
            || self.report_interval <= 10
            || self.sample_interval * 60 < self.report_interval
        {
            eprintln!("Config Error: invalid sample conf");
            exit(exitcode::CONFIG)
        }
    }
}

pub struct Sampler {
    pub source: StatSource,
    pub stats: Mutex<CircularQueue<Stats>>,
    // pub work_dir: String,
}

impl Sampler {
    pub fn sample(&self, conf: &Option<SamplerConf>) {
        let (sample_interval, report_interval) = match conf {
            Some(SamplerConf {
                sample_interval,
                report_interval,
            }) => (
                Duration::new(*sample_interval, 0),
                Duration::new(*report_interval * 60, 0),
            ),
            None => (Duration::new(15, 0), Duration::new(60 * 60, 0)),
        };

        let mut last_report = Duration::new(0, 0);
        loop {
            sleep(sample_interval);
            last_report += sample_interval;
            let (corpus, (blocks, branches), (normal_case, failed_case, crashed_case)) = (
                self.source.corpus.len(),
                self.source.feedback.len(),
                self.source.record.len(),
            );
            let stat = Stats {
                corpus,
                blocks,
                branches,
                normal_case,
                failed_case,
                crashed_case,
            };

            if report_interval <= last_report {
                self.report(&stat);
                last_report = Duration::new(0, 0);
            }
            {
                let mut stats = self.stats.lock().unwrap();
                stats.push(stat);
            }
            info!(
                "corpus {},blocks {},branches {}, normal_case {},failed_case {},crashed_case {}",
                corpus, blocks, branches, normal_case, failed_case, crashed_case
            );
        }
    }

    pub fn persist(&self) {
        let stats = {
            let stats = self.stats.lock().unwrap();
            if stats.is_empty() {
                return;
            }
            stats.asc_iter().cloned().collect::<Vec<_>>()
        };
        let path = "./stats.json";
        let stats = serde_json::to_string_pretty(&stats).unwrap();
        write(path, stats).unwrap_or_else(|e| {
            exits!(exitcode::IOERR, "Fail to persist stats to {} : {}", path, e)
        })
    }

    fn report(&self, stat: &Stats) {
        let stat = serde_json::to_string_pretty(&stat).unwrap();
        let email = EmailBuilder::new()
            .subject("Healer-Stats Regular Report")
            .body(stat);
        mail::send(email)
    }
}
