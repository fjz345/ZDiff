use std::{ffi::OsStr, io};

pub const LOG_FLAG: &str = "-log";

pub fn take_log_flag<S: AsRef<OsStr>>(args: impl IntoIterator<Item = S>) -> (bool, Vec<S>) {
    let (flags, rest): (Vec<S>, Vec<S>) = args
        .into_iter()
        .partition(|arg| arg.as_ref() == OsStr::new(LOG_FLAG));
    (!flags.is_empty(), rest)
}

#[cfg(windows)]
pub fn open_console() -> io::Result<()> {
    use std::{ffi::c_void, fs::OpenOptions, os::windows::io::IntoRawHandle};

    type Handle = *mut c_void;
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
    const STD_ERROR_HANDLE: u32 = -12i32 as u32;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const INVALID_HANDLE_VALUE: isize = -1;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
        fn AllocConsole() -> i32;
        fn GetStdHandle(std_handle: u32) -> Handle;
        fn SetStdHandle(std_handle: u32, handle: Handle) -> i32;
    }

    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        let already_attached =
            io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED);
        if !already_attached && unsafe { AllocConsole() } == 0 {
            return Err(io::Error::last_os_error());
        }
    }

    // A GUI-subsystem process gets no std handles from attaching; keep the ones that were redirected.
    let unset: Vec<u32> = [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
        .into_iter()
        .filter(|&id| {
            let handle = unsafe { GetStdHandle(id) };
            handle.is_null() || handle as isize == INVALID_HANDLE_VALUE
        })
        .collect();
    if unset.is_empty() {
        return Ok(());
    }
    let console = OpenOptions::new()
        .read(true)
        .write(true)
        .open("CONOUT$")?
        .into_raw_handle();
    for id in unset {
        if unsafe { SetStdHandle(id, console) } == 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn open_console() -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_flag_is_reported_and_removed_wherever_it_is() {
        assert_eq!(take_log_flag(os(&["-log"])), (true, os(&[])));
        assert_eq!(
            take_log_flag(os(&["a", "-log", "b"])),
            (true, os(&["a", "b"]))
        );
        assert_eq!(
            take_log_flag(os(&["a", "b", "-log"])),
            (true, os(&["a", "b"]))
        );
    }

    #[test]
    fn without_the_flag_args_are_kept_in_order() {
        assert_eq!(take_log_flag(os(&[])), (false, os(&[])));
        assert_eq!(
            take_log_flag(os(&["-nl", "x", "b", "a"])),
            (false, os(&["-nl", "x", "b", "a"]))
        );
    }

    #[test]
    fn a_repeated_flag_is_removed_every_time() {
        assert_eq!(
            take_log_flag(os(&["-log", "a", "-log"])),
            (true, os(&["a"]))
        );
    }

    #[test]
    fn only_the_exact_flag_matches() {
        let near = os(&["-logs", "--log", "/log", "log", "-LOG", "-log=1", " -log"]);
        assert_eq!(take_log_flag(near.clone()), (false, near));
    }

    #[test]
    fn string_args_work_like_os_string_args() {
        let args = vec!["-log".to_string(), "a".to_string()];
        assert_eq!(take_log_flag(args), (true, vec!["a".to_string()]));
    }

    #[cfg(windows)]
    #[test]
    fn non_utf8_args_pass_through() {
        use std::os::windows::ffi::OsStringExt;
        let lone_surrogate = OsString::from_wide(&[0xD800, b'a' as u16]);
        let args = vec![lone_surrogate.clone(), OsString::from("-log")];
        assert_eq!(take_log_flag(args), (true, vec![lone_surrogate]));
    }
}
