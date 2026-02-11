use std::fs::File;
use std::io::Write;
use memmap2::Mmap;

fn main() {
    let path = "test_empty.log";
    {
        let _f = File::create(path).unwrap();
    }
    let file = File::open(path).unwrap();
    let mmap_res = unsafe { Mmap::map(&file) };
    match mmap_res {
        Ok(_) => println!("OK"),
        Err(e) => println!("Error: {}", e),
    }
    std::fs::remove_file(path).unwrap();
}
