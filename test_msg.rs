fn main() {
    let wallclock = 1000;
    let previous = 1100;
    let drift_us = -100;
    let max_allowed = 50;
    let direction = if drift_us < 0 { "backward" } else { "forward" };
    let msg = format!(
        "Clock skew too large: system clock jumped {} by {} \u{b5}s (max allowed: {} \u{b5}s). \
         Wallclock: {}, Previous: {}. This may indicate NTP adjustment or manual clock change.",
        direction,
        drift_us.abs(),
        max_allowed,
        wallclock,
        previous
    );
    println!("{}", msg);
}
