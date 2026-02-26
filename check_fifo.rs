use std::fs::{File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};

fn main() {
    // Open for read/write to avoid blocking on open
    let mut f = OpenOptions::new().read(true).write(true).open("test_fifo").unwrap();
    println!("Opened FIFO");

    match f.seek(SeekFrom::Current(0)) {
        Ok(pos) => println!("Seek success: {}", pos),
        Err(e) => println!("Seek failed: {}", e),
    }
}
