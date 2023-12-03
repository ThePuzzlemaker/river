use crate::{asm, hart_local::LOCAL_HART};

pub fn time() -> (u64, u64) {
    let timebase_freq = LOCAL_HART.with(|hart| hart.timebase_freq.get());
    let time = asm::read_time();

    let time_ns = time * (1_000_000_000 / timebase_freq);
    let sec = time_ns / 1_000_000_000;
    let time_ns = time_ns % 1_000_000_000;
    let ms = time_ns / 1_000_000;
    (sec, ms)
}

// SPDX-License-Identifier: MPL-2.0
// SPDX-FileCopyrightText: 2021 The vanadinite developers
//
// This Source Code Form is subject to the terms of the Mozilla Public License,
// v. 2.0. If a copy of the MPL was not distributed with this file, You can
// obtain one at https://mozilla.org/MPL/2.0/.

use crate::sync::SpinRwLock;
use alloc::{collections::BTreeMap, string::String};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use log::LevelFilter;
use owo_colors::{OwoColorize, Style, Styled};

static HART_FILTER: AtomicUsize = AtomicUsize::new(usize::MAX);
static LOG_FILTER: SpinRwLock<Option<BTreeMap<String, Option<LevelFilter>>>> =
    SpinRwLock::new(None);
static LOG_LEVEL: AtomicUsize = AtomicUsize::new(LevelFilter::Info as usize);
pub static USE_COLOR: AtomicBool = AtomicBool::new(true);

pub fn parse_log_filter(filter: Option<&str>) {
    if let Some(filter) = filter {
        let mut map = BTreeMap::new();
        for part in filter.split(',') {
            let mut parts = part.split('=');
            let name = parts.next().unwrap();

            match name {
                "ignore-harts" => match parts.next() {
                    Some(list) => {
                        let mut mask = 0;
                        for n in list.split(',').filter_map(|n| n.parse::<usize>().ok()) {
                            mask |= 1 << n;
                        }

                        HART_FILTER.fetch_xor(mask, Ordering::Relaxed);
                    }
                    None => log::warn!("Missing hart list for `ignore-harts`"),
                },
                _ => {
                    if let Some(level) = level_from_str(name) {
                        set_max_level(level);
                        continue;
                    }
                }
            }

            let level = match parts.next() {
                Some(level) => match level_from_str(level) {
                    Some(level) => Some(level),
                    None => {
                        log::warn!("Bad level filter: '{}', skipping", level);
                        continue;
                    }
                },
                None => None,
            };

            map.insert(String::from(name), level);
        }

        *LOG_FILTER.write() = Some(map);
    }
}

fn level_from_str(level: &str) -> Option<LevelFilter> {
    match level {
        "off" => Some(LevelFilter::Off),
        "trace" => Some(LevelFilter::Trace),
        "debug" => Some(LevelFilter::Debug),
        "info" => Some(LevelFilter::Info),
        "warn" => Some(LevelFilter::Warn),
        "error" => Some(LevelFilter::Error),
        _ => None,
    }
}

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        let hart_id = asm::hartid();

        if HART_FILTER.load(Ordering::Relaxed) & (1 << hart_id) == 0 {
            return false;
        }

        let max_level = max_level();

        let mut mod_path = metadata.target();

        let filter = LOG_FILTER.read();
        match &*filter {
            Some(filters) => {
                let mod_filter = filters.iter().find(|(k, _)| mod_path.starts_with(*k));

                match mod_filter {
                    Some((_, Some(level))) => metadata.level() <= *level,
                    _ if metadata.level() <= max_level => true,
                    _ => false,
                }
            }
            None => metadata.level() <= max_level,
        }
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            let mut mod_path = record
                .module_path_static()
                .or_else(|| record.module_path())
                .unwrap_or("<n/a>");

            let color = match record.level() {
                log::Level::Trace => Style::new().bright_magenta(),
                log::Level::Debug => Style::new().blue(),
                log::Level::Info => Style::new().green(),
                log::Level::Warn => Style::new().yellow(),
                log::Level::Error => Style::new().red(),
            }
            .bold();

            let lvl = match record.level() {
                log::Level::Trace => "trac",
                log::Level::Debug => "debg",
                log::Level::Info => "info",
                log::Level::Warn => "WARN",
                log::Level::Error => "CRIT",
            };

            let (sec, ms) = time();
            let hartid = asm::hartid();
            crate::println!(
                "{}{:>5}.{:<03}{} {}{}{} {}{}{}{} {}{} {}",
                "[".white().dimmed(),
                sec.white().dimmed(),
                ms.white().dimmed(),
                "]".white().dimmed(),
                "[".white().dimmed(),
                hartid.white().dimmed(),
                "]".white().dimmed(),
                "[".style(color),
                lvl.style(color),
                "]".style(color),
                ":".white().bold(),
                mod_path.white().bold(),
                ":".white().bold(),
                record.args()
            );
        }
    }

    fn flush(&self) {}
}

pub fn init_logging() {
    log::set_logger(&Logger).expect("failed to init logging");
    log::set_max_level(log::LevelFilter::Trace);
}

fn max_level() -> LevelFilter {
    unsafe { core::mem::transmute(LOG_LEVEL.load(Ordering::Relaxed)) }
}

fn set_max_level(filter: LevelFilter) {
    LOG_LEVEL.store(filter as usize, Ordering::Relaxed)
}
