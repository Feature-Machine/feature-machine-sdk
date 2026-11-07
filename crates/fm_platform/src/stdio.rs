// A helper that loads creator code keeps a private copy of its inherited stdout for its own
// protocol, then points stdout at the null device (Win32 handle and C runtime fd 1). Feature
// prints then cannot corrupt the stream or block on a pipe that the parent reads only per reply.
use std::{fs::File, io::{self, Write}};

/// Returns the private stream. Call it before any creator code loads; a later print of this
/// process (Rust or C) writes to the null device.
pub fn take_stdout() -> io::Result<File> {
    io::stdout().flush()?;
    imp::take()
}

#[cfg(windows)]
mod imp {
    use std::{fs::File, io, os::windows::io::{AsHandle, IntoRawHandle}};
    use windows_sys::Win32::System::Console::{SetStdHandle, STD_OUTPUT_HANDLE};
    // The C runtime that Rust links (UCRT) keeps its own table of descriptors.
    extern "C" {
        fn _open_osfhandle(handle: isize, flags: i32) -> i32;
        fn _dup2(from: i32, to: i32) -> i32;
        fn _close(fd: i32) -> i32;
    }
    const O_WRONLY: i32 = 1;

    pub fn take() -> io::Result<File> {
        let private = File::from(io::stdout().as_handle().try_clone_to_owned()?);
        let null = std::fs::OpenOptions::new().write(true).open("NUL")?;
        let for_crt = null.try_clone()?.into_raw_handle();
        // SAFETY: the null handles stay open for the process life (into_raw_handle, never closed
        // here except by the CRT that owns `fd`); the descriptors are plain integers.
        unsafe {
            if SetStdHandle(STD_OUTPUT_HANDLE, null.into_raw_handle()) == 0 { return Err(io::Error::last_os_error()); }
            let fd = _open_osfhandle(for_crt as isize, O_WRONLY);
            if fd < 0 { return Err(io::Error::other("the C runtime cannot open the null device")); }
            let moved = _dup2(fd, 1);
            _close(fd);
            if moved != 0 { return Err(io::Error::other("the C runtime stdout cannot move to the null device")); }
        }
        Ok(private)
    }
}

#[cfg(unix)]
mod imp {
    use std::{fs::File, io, os::fd::{AsFd, AsRawFd}};

    pub fn take() -> io::Result<File> {
        let private = File::from(io::stdout().as_fd().try_clone_to_owned()?);
        let null = std::fs::OpenOptions::new().write(true).open("/dev/null")?;
        // SAFETY: both descriptors are open; dup2 replaces fd 1 atomically.
        if unsafe { libc::dup2(null.as_raw_fd(), 1) } < 0 { return Err(io::Error::last_os_error()); }
        Ok(private)
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    // The test runs itself again as a child: only the private stream may reach the pipe.
    #[test]
    fn prints_after_the_take_cannot_reach_the_private_stream() {
        if std::env::var_os("FM_STDIO_CHILD").is_some() {
            let mut private = super::take_stdout().unwrap();
            println!("rust noise");
            #[cfg(windows)]
            {
                extern "C" { fn _write(fd: i32, buf: *const u8, n: u32) -> i32; }
                // SAFETY: a static buffer of the given length.
                unsafe { _write(1, b"crt noise\n".as_ptr(), 10) };
            }
            #[cfg(unix)]
            // SAFETY: a static buffer of the given length.
            unsafe { libc::write(1, b"crt noise\n".as_ptr().cast(), 10) };
            private.write_all(b"control line\n").unwrap();
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "stdio::tests::prints_after_the_take_cannot_reach_the_private_stream", "--nocapture", "--test-threads=1"])
            .env("FM_STDIO_CHILD", "1").output().unwrap();
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{text}");
        assert!(text.contains("control line\n") && !text.contains("noise"), "{text}");
    }
}
