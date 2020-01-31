use bitcoin::TxOut;
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

pub struct ScriptVerification {
    pub previous_output: TxOut,
    pub raw_tx: Arc<Vec<u8>>,
    pub index: usize,
    pub flags: u32,
}

impl ScriptVerification {
    fn check(&self) -> bool {
        bitcoinconsensus::verify_with_flags(
            self.previous_output.script_pubkey.as_bytes(),
            self.previous_output.value,
            &self.raw_tx,
            self.index as usize,
            self.flags,
        )
        .is_ok()
    }
}
pub struct CheckQueue {
    state: Arc<Mutex<CheckQueueState>>,
    cond_worker: Arc<Condvar>,
    cond_master: Arc<Condvar>,
    batch_size: usize,
}

impl CheckQueue {
    pub fn new(batch_size: usize) -> Self {
        Self {
            state: Arc::new(Mutex::new(CheckQueueState::new())),
            cond_worker: Default::default(),
            cond_master: Default::default(),
            batch_size,
        }
    }

    pub fn run(&self, master: bool) -> std::thread::JoinHandle<bool> {
        let cond = if master {
            self.cond_master.clone()
        } else {
            self.cond_worker.clone()
        };
        let mut checks = Vec::with_capacity(self.batch_size);
        let mut now = None;
        let mut ok = true;
        let batch_size = self.batch_size;
        let cond_master = self.cond_master.clone();
        let state = self.state.clone();

        std::thread::spawn(move || loop {
            {
                let mut state = state.lock().unwrap();
                if let Some(now) = now {
                    state.all_ok &= ok;
                    state.todo -= now;
                    if state.todo == 0 && !master {
                        cond_master.notify_one();
                    }
                } else {
                    state.total += 1;
                }
                while state.queue.is_empty() {
                    if master && state.todo == 0 {
                        state.total -= 1;
                        let ret = state.all_ok;
                        if master {
                            state.all_ok = true;
                        }
                        return ret;
                    }
                    state.idle += 1;
                    state = cond.wait(state).unwrap();
                    state.idle -= 1;
                }
                now = Some(std::cmp::max(
                    1,
                    std::cmp::min(
                        batch_size,
                        state.queue.len() / (state.total + state.idle + 1),
                    ),
                ));
                for _ in 0..now.unwrap() {
                    checks.push(state.queue.pop_back().unwrap());
                }
                ok = state.all_ok;
            }
            for check in &mut checks {
                if ok {
                    ok = check.check();
                }
            }
            checks.clear();
        })
    }

    pub fn thread(&self) {
        self.run(false);
    }

    pub fn wait(&self) -> bool {
        self.run(true).join().unwrap()
    }

    pub fn add(&self, checks: Vec<ScriptVerification>) {
        let checks_len = checks.len();
        {
            let mut state = self.state.lock().unwrap();
            for check in checks {
                state.queue.push_back(check);
            }
            state.todo += checks_len;
        }

        use std::cmp::Ordering::*;
        match checks_len.cmp(&1) {
            Greater => self.cond_worker.notify_all(),
            Equal => self.cond_worker.notify_one(),
            Less => (),
        }
    }
}

pub struct CheckQueueState {
    queue: VecDeque<ScriptVerification>,
    idle: usize,
    total: usize,
    all_ok: bool,
    todo: usize,
}

impl CheckQueueState {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::default(),
            idle: 0,
            total: 0,
            all_ok: true,
            todo: 0,
        }
    }
}

pub struct CheckQueueControl {
    queue: CheckQueue,
    done: bool,
}

impl CheckQueueControl {
    pub fn new(queue: CheckQueue) -> Self {
        Self { queue, done: false }
    }

    pub fn wait(&mut self) -> bool {
        let ret = self.queue.wait();
        self.done = true;
        ret
    }

    pub fn add(&self, checks: Vec<ScriptVerification>) {
        self.queue.add(checks)
    }
}

impl Drop for CheckQueueControl {
    fn drop(&mut self) {
        if !self.done {
            self.wait();
        }
    }
}
