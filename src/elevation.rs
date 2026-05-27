use std::env;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

#[link(name = "shell32")]
unsafe extern "system" {
    fn IsUserAnAdmin() -> bool;

    fn ShellExecuteW(
        hwnd: *mut std::ffi::c_void,
        lp_operation: *const u16,
        lp_file: *const u16,
        lp_parameters: *const u16,
        lp_directory: *const u16,
        n_show_cmd: i32,
    ) -> isize;
}

pub enum ElevationStatus {
    Ready,
    Relaunched,
    Failed,
}

pub fn ensure_admin() -> ElevationStatus {
    if unsafe { IsUserAnAdmin() } {
        return ElevationStatus::Ready;
    }

    let exe = match env::current_exe() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("failed to find current executable for admin relaunch: {error}");
            return ElevationStatus::Failed;
        }
    };

    let params = env::args()
        .skip(1)
        .map(|arg| quote_arg(&arg))
        .collect::<Vec<_>>()
        .join(" ");

    let operation = wide_null("runas");
    let file = wide_null(exe.as_os_str());
    let parameters = wide_null(OsStr::new(&params));
    let directory = exe
        .parent()
        .map(|path| wide_null(path.as_os_str()))
        .unwrap_or_else(|| wide_null(""));

    // ShellExecuteW returns a value > 32 on success. goofy ahh windows api behavior
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            directory.as_ptr(),
            1,
        )
    };

    if result <= 32 {
        eprintln!("admin relaunch failed or was cancelled. ShellExecuteW returned {result}.");
        return ElevationStatus::Failed;
    }

    ElevationStatus::Relaunched
}

fn wide_null(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn quote_arg(arg: &str) -> String {
    if arg.contains(' ') || arg.contains('"') {
        format!("\"{}\"", arg.replace('"', "\\\""))
    } else {
        arg.to_string()
    }
}
