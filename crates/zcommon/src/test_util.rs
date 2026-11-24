/// `C:\ws\src` on Windows, `/ws/src` elsewhere: one test path literal for every platform.
pub fn os_path(windows: &str) -> String {
    if cfg!(windows) {
        return windows.to_owned();
    }
    let without_drive = match windows.as_bytes() {
        [drive, b':', ..] if drive.is_ascii_alphabetic() => &windows[2..],
        _ => windows,
    };
    without_drive.replace('\\', "/")
}
