use crate::{asm, hart_local::LOCAL_HART};

#[macro_export]
macro_rules! info {
    ($($tt:tt)*) => {{
	use owo_colors::OwoColorize;
	let (sec, ms) = $crate::syslog::time();
	let hartid = $crate::asm::hartid();
        $crate::print!("{}{:>5}.{:<03}{} {}{}{} {}{} ", "[".white().dimmed(), sec.white().dimmed(), ms.white().dimmed(), "]".white().dimmed(),  "[".white().dimmed(), hartid.white().dimmed(), "]".white().dimmed(),"[info]".green().bold(), ":".white().bold());
	$crate::println!($($tt)*);
    }};
}

#[macro_export]
macro_rules! critical {
    ($($tt:tt)*) => {{
	use owo_colors::OwoColorize;
	let (sec, ms) = $crate::syslog::time();
	let hartid = $crate::asm::hartid();
        $crate::print!("{}{:>5}.{:<03}{} {}{}{} {}{} ", "[".white().dimmed(), sec.white().dimmed(), ms.white().dimmed(), "]".white().dimmed(),  "[".white().dimmed(), hartid.white().dimmed(), "]".white().dimmed(),"[crit]".red().bold(), ":".white().bold());
	$crate::println!($($tt)*);
    }};
}

pub fn time() -> (u64, u64) {
    let timebase_freq = LOCAL_HART.with(|hart| hart.timebase_freq.get());
    let time = asm::read_time();

    let time_ns = time * (1_000_000_000 / timebase_freq);
    let sec = time_ns / 1_000_000_000;
    let time_ns = time_ns % 1_000_000_000;
    let ms = time_ns / 1_000_000;
    (sec, ms)
}
