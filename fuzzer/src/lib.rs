#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate serde;
#[macro_use]
extern crate log;

use crate::corpus::Corpus;
use crate::exec::{Executor, ExecutorConf};
use crate::feedback::FeedBack;
use crate::fuzzer::Fuzzer;
use crate::guest::{GuestConf, QemuConf, SSHConf};
use crate::mail::MailConf;
use crate::report::TestCaseRecord;

use circular_queue::CircularQueue;
use core::analyze::static_analyze;
use core::prog::Prog;
use core::target::Target;
use fots::types::Items;
use std::fs::{create_dir_all, read, write};
use std::process;
use std::sync::Barrier;
use std::sync::Mutex;
use std::sync::{Arc, RwLock};
use std::thread::spawn;

#[macro_use]
pub mod utils;
pub mod corpus;
pub mod exec;
#[allow(dead_code)]
pub mod feedback;
pub mod fuzzer;
pub mod guest;
pub mod mail;
pub mod report;
pub mod stats;

use crate::stats::SamplerConf;
use stats::StatSource;
use std::path::PathBuf;
use std::process::exit;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub fots_bin: PathBuf,
    pub corpus: Option<PathBuf>,
    pub vm_num: usize,
    pub auto_reboot_duration: Option<u64>,

    pub guest: GuestConf,
    pub qemu: Option<QemuConf>,
    pub ssh: Option<SSHConf>,

    pub executor: ExecutorConf,

    pub mail: Option<MailConf>,
    pub sampler: Option<SamplerConf>,
}

impl Config {
    pub fn check(&self) {
        if !self.fots_bin.is_file() {
            eprintln!(
                "Config Error: fots file {} not exists",
                self.fots_bin.display()
            );
            exit(exitcode::CONFIG)
        }
        if let Some(corpus) = &self.corpus {
            if !corpus.is_file() {
                eprintln!(
                    "Config Error: corpus file {} not exists",
                    self.fots_bin.display()
                );
                exit(exitcode::CONFIG)
            }
        }

        let cpu_num = num_cpus::get();
        if self.vm_num == 0 || self.vm_num > cpu_num {
            eprintln!(
                "Config Error: invalid vm num {}, vm num must between (0,{}] on your system",
                self.vm_num, cpu_num
            );
            exit(exitcode::CONFIG)
        }
        self.guest.check();
        self.executor.check();
        if let Some(qemu) = self.qemu.as_ref() {
            qemu.check()
        }
        if let Some(ssh) = self.ssh.as_ref() {
            ssh.check()
        }
        if let Some(mail) = self.mail.as_ref() {
            mail.check()
        }
        if let Some(sampler) = self.sampler.as_ref() {
            sampler.check()
        }
    }
}

pub fn start_up(cfg: Config) {
    let target = load_target(&cfg);
    let progs = load_candidates(&cfg.corpus);
    info!("Corpus: {}", progs.len());
    if let Some(mail_conf) = cfg.mail.as_ref() {
        info!("Email report to: {:?}", mail_conf.receivers);
    }

    // shared between multi tasks
    let target = Arc::new(target);
    let corpus = Arc::new(Corpus::default());
    let feedback = Arc::new(FeedBack::default());
    let record = Arc::new(TestCaseRecord::new(target.clone()));
    let rt = Arc::new(RwLock::new(static_analyze(&target)));

    let barrier = Arc::new(Barrier::new(cfg.vm_num + 1));
    info!(
        "Booting {} {}/{} on {} ...",
        cfg.vm_num, cfg.guest.os, cfg.guest.arch, cfg.guest.platform
    );
    let now = std::time::Instant::now();

    for _ in 0..cfg.vm_num {
        let fuzzer = Fuzzer {
            rt: rt.clone(),
            target: target.clone(),
            conf: Default::default(),
            corpus: corpus.clone(),
            feedback: feedback.clone(),
            record: record.clone(),
        };
        let barrier = barrier.clone();
        let mut executor = Executor::new(&cfg);
        let progs = progs.clone();
        let cfg = cfg.clone();

        spawn(move || {
            executor.start();
            barrier.wait();
            fuzzer.fuzz(executor, progs, cfg);
        });
    }

    barrier.wait();
    info!("Boot finished, cost {}s.", now.elapsed().as_secs());
    let sampler = Arc::new(stats::Sampler {
        source: StatSource {
            corpus: corpus.clone(),
            feedback,
            record: record.clone(),
        },
        stats: Mutex::new(CircularQueue::with_capacity(1024)),
    });

    let sampler_ = sampler.clone();
    spawn(move || {
        use signal_hook::{iterator::Signals, SIGINT, SIGTERM};
        let sigs = Signals::new(&[SIGINT, SIGTERM]).unwrap();
        for sig in sigs.forever() {
            warn!("sig-{} received, persisting data...", sig);

            let corpus_path = "./corpus".to_string();
            let corpus = corpus
                .dump()
                .unwrap_or_else(|e| exits!(exitcode::DATAERR, "Fail to dump corpus: {}", e));
            write(&corpus_path, corpus).unwrap_or_else(|e| {
                exits!(
                    exitcode::IOERR,
                    "Fail to persist corpus to {} : {}",
                    corpus_path,
                    e
                )
            });
            record.psersist();
            sampler_.persist();
            exit(exitcode::OK)
        }
    });

    sampler.sample(&cfg.sampler);
}

fn load_candidates(path: &Option<PathBuf>) -> Vec<Prog> {
    if let Some(path) = path.as_ref() {
        let data = read(path).unwrap();
        bincode::deserialize(&data).unwrap()
    } else {
        Vec::default()
    }
}

fn load_target(cfg: &Config) -> Target {
    let items = Items::load(&read(&cfg.fots_bin).unwrap_or_else(|e| {
        error!("Fail to load fots file: {}", e);
        exit(exitcode::DATAERR);
    }))
    .unwrap();
    // split(&mut items, cfg.vm_num)
    Target::from(items)
}

pub fn init_env() {
    use std::io::ErrorKind;

    // pretty_env_logger::init_timed();
    init_logger();
    let pid = process::id();
    std::env::set_var("HEALER_FUZZER_PID", format!("{}", pid));
    info!("Pid: {}", pid);

    // let work_dir = std::env::var("HEALER_WORK_DIR").unwrap_or_else(|_| String::from("."));
    // std::env::set_var("HEALER_WORK_DIR", &work_dir);
    // info!("Work-dir: {}", work_dir);

    if let Err(e) = create_dir_all("./crashes") {
        if e.kind() != ErrorKind::AlreadyExists {
            exits!(exitcode::IOERR, "Fail to create crash dir: {}", e);
        }
    }
}

fn init_logger() {
    use log::LevelFilter;
    use log4rs::append::console::ConsoleAppender;
    use log4rs::append::file::FileAppender;
    use log4rs::append::rolling_file::policy::compound::{roll, trigger, CompoundPolicy};
    use log4rs::append::rolling_file::RollingFileAppender;
    use log4rs::config::{Appender, Config, Logger, Root};
    use log4rs::encode::pattern::PatternEncoder;

    let stdout = ConsoleAppender::builder()
        .encoder(Box::new(PatternEncoder::new(
            "{d(%Y-%m-%d %H:%M:%S)} {h({l})} {t} - {m}{n}",
        )))
        .build();

    let fuzzer_appender = FileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(
            "{d(%Y-%m-%d %H:%M:%S)} {h({l})} - {m}{n}",
        )))
        .build("log/fuzzer.log")
        .unwrap();

    let stats_trigger = trigger::size::SizeTrigger::new(1024 * 1024 * 100);
    let stats_roll = roll::fixed_window::FixedWindowRoller::builder()
        .build("stats.log.{}", 2)
        .unwrap();
    let stats_policy = CompoundPolicy::new(Box::new(stats_trigger), Box::new(stats_roll));
    let stats_appender = RollingFileAppender::builder()
        .append(false)
        .encoder(Box::new(PatternEncoder::new(
            "{d(%Y-%m-%d %H:%M:%S)} {h({l})} - {m}{n}",
        )))
        .build("log/stats.log", Box::new(stats_policy))
        .unwrap();

    let config = Config::builder()
        .appender(Appender::builder().build("stdout", Box::new(stdout)))
        .appender(Appender::builder().build("fuzzer_appender", Box::new(fuzzer_appender)))
        .appender(Appender::builder().build("stats_appender", Box::new(stats_appender)))
        .logger(
            Logger::builder()
                .appender("stats_appender")
                .build("fuzzer::stats", LevelFilter::Info),
        )
        .logger(
            Logger::builder()
                .appender("fuzzer_appender")
                .build("fuzzer::fuzzer", LevelFilter::Info),
        )
        .build(Root::builder().appender("stdout").build(LevelFilter::Info))
        .unwrap();
    log4rs::init_config(config).unwrap();
}

// fn split(items: &mut Items, n: usize) -> Vec<Target> {
//     assert!(items.groups.len() > n);
//
//     let mut result = Vec::new();
//     let total = items.groups.len();
//
//     for n in Split::new(total, n) {
//         let sub_groups = items.groups.drain(items.groups.len() - n..);
//         let target = Target::from(Items {
//             types: items.types.clone(),
//             groups: sub_groups.collect(),
//             rules: vec![],
//         });
//         result.push(target);
//     }
//     result
// }

const HEALER: &str = r"
 ___   ___   ______   ________   __       ______   ______
/__/\ /__/\ /_____/\ /_______/\ /_/\     /_____/\ /_____/\
\::\ \\  \ \\::::_\/_\::: _  \ \\:\ \    \::::_\/_\:::_ \ \
 \::\/_\ .\ \\:\/___/\\::(_)  \ \\:\ \    \:\/___/\\:(_) ) )_
  \:: ___::\ \\::___\/_\:: __  \ \\:\ \____\::___\/_\: __ `\ \
   \: \ \\::\ \\:\____/\\:.\ \  \ \\:\/___/\\:\____/\\ \ `\ \ \
    \__\/ \::\/ \_____\/ \__\/\__\/ \_____\/ \_____\/ \_\/ \_\/

";

pub fn show_info() {
    println!("{}", HEALER);
}
