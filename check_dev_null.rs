use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::io::AsRawFd;

fn main() {
    let mut f = File::open("/dev/null").unwrap();
    println!("Opened /dev/null");

    match f.seek(SeekFrom::Current(0)) {
        Ok(pos) => println!("Seek success: {}", pos),
        Err(e) => println!("Seek failed: {}", e),
    }

    match f.set_len(0) {
        Ok(_) => println!("set_len success"),
        Err(e) => println!("set_len failed: {}", e),
    }
}
