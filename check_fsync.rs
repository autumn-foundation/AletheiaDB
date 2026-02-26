use std::os::unix::net::UnixStream;
use std::fs::File;
use std::os::unix::io::{FromRawFd, IntoRawFd};

fn main() {
    let (s1, _s2) = UnixStream::pair().unwrap();
    let fd = s1.into_raw_fd();
    let file = unsafe { File::from_raw_fd(fd) };

    let res = file.sync_data();
    println!("fsync result: {:?}", res);
}
