use crate::util::now;
use std::collections::HashSet;
use std::net::SocketAddr;

pub struct TimeData {
    offset: i64,
    samples: Vec<i64>,
    known: HashSet<SocketAddr>,
}

impl Default for TimeData {
    fn default() -> Self {
        Self {
            offset: 0,
            samples: vec![0],
            known: HashSet::default(),
        }
    }
}

impl TimeData {
    const BITCOIN_TIMEDATA_MAX_SAMPLES: usize = 200;
    const DEFAULT_MAX_TIME_ADJUSTMENT: usize = 70 * 60;

    pub fn get_time_offset(&self) -> i64 {
        self.offset
    }

    pub fn get_adjusted_time(&self) -> u64 {
        let now = now() as i64;
        let adjusted = now + self.offset;
        assert!(adjusted >= 0);
        adjusted as u64
    }

    pub fn add_time_data(&mut self, addr: SocketAddr, offset: i64) {
        if self.samples.len() == TimeData::BITCOIN_TIMEDATA_MAX_SAMPLES {
            return;
        }
        if !self.known.insert(addr) {
            return;
        }
        self.input(offset);
        if self.samples.len() >= 5 && self.samples.len() % 2 == 1 {
            let median = self.median();
            if median.abs() <= TimeData::DEFAULT_MAX_TIME_ADJUSTMENT as i64 {
                self.offset = median;
            } else {
                self.offset = 0;
            }
        }
    }

    // doesn't take into account size
    fn input(&mut self, sample: i64) {
        self.samples.push(sample);
        self.samples.sort();
    }

    fn median(&self) -> i64 {
        let len = self.samples.len();
        if len & 1 == 1 {
            self.samples[len / 2]
        } else {
            (self.samples[(len / 2) - 1] + self.samples[len / 2]) / 2
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::net::SocketAddr;

    #[test]
    fn test_median() {
        let mut time_data = TimeData::default();
        time_data.samples.clear();

        time_data.input(15);
        assert_eq!(time_data.median(), 15);

        time_data.input(20);
        assert_eq!(time_data.median(), 17);

        time_data.input(30);
        assert_eq!(time_data.median(), 20);

        time_data.input(3);
        assert_eq!(time_data.median(), 17);

        time_data.input(7);
        assert_eq!(time_data.median(), 15);
    }

    fn multi_add_time_data(
        time_data: &mut TimeData,
        port: &mut u16,
        addr: &mut SocketAddr,
        n: usize,
        offset: i64,
    ) {
        for _ in 0..n {
            addr.set_port(*port);
            *port += 1;
            time_data.add_time_data(*addr, offset);
        }
    }

    #[test]
    fn test_add() {
        let mut time_data = TimeData::default();
        let mut addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let mut port = 0;
        let max = TimeData::DEFAULT_MAX_TIME_ADJUSTMENT as i64;

        assert_eq!(time_data.get_time_offset(), 0);

        multi_add_time_data(&mut time_data, &mut port, &mut addr, 3, max + 1);
        multi_add_time_data(&mut time_data, &mut port, &mut addr, 1, max + 1);
        assert_eq!(time_data.get_time_offset(), 0);

        multi_add_time_data(&mut time_data, &mut port, &mut addr, 4, 100);
        assert_eq!(time_data.get_time_offset(), 100);

        multi_add_time_data(&mut time_data, &mut port, &mut addr, 10, -100);
        assert_eq!(time_data.get_time_offset(), -100);

        multi_add_time_data(&mut time_data, &mut port, &mut addr, 89, 100);
        multi_add_time_data(&mut time_data, &mut port, &mut addr, 89, -100);
        assert_eq!(time_data.get_time_offset(), -100);

        multi_add_time_data(&mut time_data, &mut port, &mut addr, 2, 100);
        assert_eq!(time_data.get_time_offset(), 0);

        multi_add_time_data(&mut time_data, &mut port, &mut addr, 2, 100);
        assert_eq!(time_data.get_time_offset(), 0);
    }
}
