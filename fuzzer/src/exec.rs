use crate::guest;
use crate::guest::{Crash, Guest};
use crate::utils::cli::{App, Arg, OptVal};
use crate::Config;
use core::prog::Prog;
use executor::transfer::{recv_result, send, Error};
use executor::{ExecResult, Reason};
use rayon_core::spawn;
use std::io::{ErrorKind, Read};
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::exit;
use std::process::Child;
use std::sync::mpsc::sync_channel;
use std::thread::sleep;
use std::time::Duration;

// config for executor
#[derive(Debug, Deserialize, Clone)]
pub struct ExecutorConf {
    pub path: PathBuf,
    pub host_ip: Option<String>,
    pub concurrency: bool,
    pub memleak_check: bool,
}

impl ExecutorConf {
    pub fn check(&self) {
        if !self.path.is_file() {
            eprintln!(
                "Config Error: executor executable file {} not exists",
                self.path.display()
            );
            exit(exitcode::CONFIG)
        }
        if let Some(ip) = &self.host_ip {
            let addr = format!("{}:8080", ip);
            if let Err(e) = addr.to_socket_addrs() {
                eprintln!(
                    "Config Error: invalid host ip `{}`: {}",
                    self.host_ip.as_ref().unwrap(),
                    e
                );
                exit(exitcode::CONFIG)
            }
        }
    }
}

pub struct Executor {
    inner: ExecutorImpl,
}

enum ExecutorImpl {
    Linux(LinuxExecutor),
}

impl Executor {
    pub fn new(cfg: &Config) -> Self {
        Self {
            inner: ExecutorImpl::Linux(LinuxExecutor::new(cfg)),
        }
    }

    pub fn start(&mut self) {
        match self.inner {
            ExecutorImpl::Linux(ref mut e) => e.start(),
        }
    }

    pub fn exec(&mut self, p: &Prog) -> Result<ExecResult, Crash> {
        match self.inner {
            ExecutorImpl::Linux(ref mut e) => e.exec(p),
        }
    }
}

struct LinuxExecutor {
    guest: Guest,
    port: u16,
    exec_handle: Option<Child>,
    conn: Option<TcpStream>,
    concurrency: bool,
    memleak_check: bool,
    executor_bin_path: PathBuf,
    target_path: PathBuf,
    host_ip: String,
}

impl LinuxExecutor {
    pub fn new(cfg: &Config) -> Self {
        let guest = Guest::new(cfg);
        let port = port_check::free_local_port()
            .unwrap_or_else(|| exits!(exitcode::TEMPFAIL, "No Free port for executor driver"));
        let host_ip = cfg
            .executor
            .host_ip
            .as_ref()
            .map(String::from)
            .unwrap_or_else(|| String::from(guest::LINUX_QEMU_HOST_IP_ADDR));

        Self {
            guest,
            port,
            exec_handle: None,
            conn: None,

            concurrency: cfg.executor.concurrency,
            memleak_check: cfg.executor.memleak_check,
            executor_bin_path: cfg.executor.path.clone(),
            target_path: cfg.fots_bin.clone(),
            host_ip,
        }
    }

    pub fn start(&mut self) {
        // handle should be set to kill on drop
        self.exec_handle = None;
        self.guest.boot();
        self.start_executer()
    }

    fn start_executer(&mut self) {
        let target = self.guest.copy(&self.target_path);
        let (tx, rx) = sync_channel(1);
        let host_addr = format!("{}:{}", self.host_ip, self.port);

        let mut executor = App::new(self.executor_bin_path.to_str().unwrap())
            .arg(Arg::new_opt("-t", OptVal::normal(target.to_str().unwrap())))
            .arg(Arg::new_opt(
                "-a",
                OptVal::normal(&format!(
                    "{}:{}",
                    guest::LINUX_QEMU_USER_NET_HOST_IP_ADDR,
                    self.port
                )),
            ));
        if self.memleak_check {
            executor = executor.arg(Arg::new_flag("-m"));
        }
        if self.concurrency {
            executor = executor.arg(Arg::new_flag("-c"));
        }

        spawn(move || {
            let listener = TcpListener::bind(&host_addr).unwrap_or_else(|e| {
                exits!(exitcode::OSERR, "Fail to listen on {}: {}", host_addr, e)
            });
            listener
                .set_nonblocking(true)
                .expect("Cannot set non-blocking");
            let mut try_time = 0;
            loop {
                match listener.accept() {
                    Ok((conn, addr)) => {
                        info!("connected from: {}", addr);
                        tx.send(conn).unwrap();
                        break;
                    }
                    Err(e) => {
                        if e.kind() == ErrorKind::WouldBlock {
                            if try_time <= 50 {
                                sleep(Duration::from_millis(100));
                                try_time += 1;
                            } else {
                                error!("Wait timeout of executor connection");
                                exit(exitcode::IOERR)
                            }
                        } else {
                            error!("Fail to wait executor connection: {}", e);
                            exit(exitcode::IOERR)
                        }
                    }
                }
            }
        });

        self.exec_handle = Some(self.guest.run_cmd(&executor));
        let conn = rx.recv().unwrap();
        conn.set_write_timeout(Some(Duration::new(20, 0))).unwrap();
        conn.set_read_timeout(Some(Duration::new(20, 0))).unwrap();
        self.conn = Some(conn);
    }

    pub fn exec(&mut self, p: &Prog) -> Result<ExecResult, Crash> {
        // send must be success
        assert!(self.conn.is_some());
        if let Err(e) = send(p, self.conn.as_mut().unwrap()) {
            match e {
                Error::Io(e) => {
                    if e.kind() == ErrorKind::WouldBlock {
                        info!("Prog send blocked, restarting...");
                        self.start();
                        return Ok(ExecResult::Failed(Reason("Prog send blocked".into())));
                    } else {
                        eprintln!("Fail to send prog: {}", e);
                        exit(exitcode::OSERR);
                    }
                }
                Error::Serialize(e) => {
                    eprintln!("Fail to serialize prog: {}", e);
                    exit(exitcode::SOFTWARE);
                }
            }
        }

        match recv_result(self.conn.as_mut().unwrap()) {
            Ok(result) => {
                self.guest.clear();
                if self.memleak_check {
                    if let ExecResult::Failed(ref reason) = result {
                        let rea = reason.to_string();
                        if rea.contains("CRASH-MEMLEAK") {
                            return Err(Crash { inner: rea });
                        }
                    }
                }
                Ok(result)
            }
            Err(e) => {
                match e {
                    Error::Io(e) => {
                        if e.kind() == ErrorKind::WouldBlock {
                            info!("Prog recv blocked, restarting...");
                            self.start();
                            return Ok(ExecResult::Failed(Reason("Prog send blocked".into())));
                        }
                    }
                    Error::Serialize(e) => {
                        eprintln!("Fail to deserialize recv: {}", e);
                        exit(exitcode::SOFTWARE);
                    }
                }

                if !self.guest.is_alive() {
                    Err(self.guest.collect_crash())
                } else {
                    // executor crashed
                    let mut handle = self.exec_handle.take().unwrap();
                    let mut stdout = handle.stdout.take().unwrap();
                    let mut stderr = handle.stderr.take().unwrap();
                    handle.wait().unwrap_or_else(|e| {
                        exits!(exitcode::OSERR, "Fail to wait executor handle:{}", e)
                    });

                    let mut err = Vec::new();
                    stderr.read_to_end(&mut err).unwrap();
                    let mut out = Vec::new();
                    stdout.read_to_end(&mut out).unwrap();

                    warn!(
                        "Executor: Connection lost. STDOUT:{}. STDERR: {}",
                        String::from_utf8(out).unwrap(),
                        String::from_utf8(err).unwrap()
                    );
                    self.start_executer();
                    Ok(ExecResult::Failed(Reason("Executor crashed".into())))
                }
            }
        }
    }
}
