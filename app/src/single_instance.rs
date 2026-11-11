//! Single-instance enforcement for Windows: ensures only one instance of Mycraft runs.
//! If the user clicks the executable or shortcut while an instance is already running
//! (including when minimized to the system tray), the running instance unhides its window,
//! restores it, and brings it to the foreground.

use std::sync::Mutex;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, HANDLE,
};
#[cfg(windows)]
use windows_sys::Win32::Security::{
    InitializeSecurityDescriptor, SetSecurityDescriptorDacl, PSECURITY_DESCRIPTOR,
    SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    CreateEventW, CreateMutexW, OpenEventW, SetEvent, EVENT_MODIFY_STATE,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AllowSetForegroundWindow, BringWindowToTop, FindWindowW, IsIconic, SetForegroundWindow,
    ShowWindow, SW_RESTORE,
};

#[cfg(windows)]
pub struct SingleInstanceGuard {
    mutex: HANDLE,
    event: HANDLE,
}

#[cfg(windows)]
unsafe impl Send for SingleInstanceGuard {}
#[cfg(windows)]
unsafe impl Sync for SingleInstanceGuard {}

#[cfg(windows)]
impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        unsafe {
            if !self.event.is_null() {
                CloseHandle(self.event);
                self.event = std::ptr::null_mut();
            }
            if !self.mutex.is_null() {
                CloseHandle(self.mutex);
                self.mutex = std::ptr::null_mut();
            }
        }
    }
}

#[cfg(windows)]
static GUARD: Mutex<Option<SingleInstanceGuard>> = Mutex::new(None);

pub enum InstanceStatus {
    Primary,
    AlreadyRunning,
}

#[cfg(windows)]
pub fn acquire_or_activate() -> InstanceStatus {
    // Build a SECURITY_ATTRIBUTES with a NULL DACL so that processes across different
    // integrity levels (e.g. non-admin and admin) can open and signal the mutex/event.
    let mut sd = std::mem::MaybeUninit::<SECURITY_DESCRIPTOR>::uninit();
    let mut sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: std::ptr::null_mut(),
        bInheritHandle: 0,
    };
    unsafe {
        if InitializeSecurityDescriptor(
            sd.as_mut_ptr() as PSECURITY_DESCRIPTOR,
            1, // SECURITY_DESCRIPTOR_REVISION
        ) != 0
        {
            if SetSecurityDescriptorDacl(
                sd.as_mut_ptr() as PSECURITY_DESCRIPTOR,
                1,                    // TRUE
                std::ptr::null_mut(), // NULL DACL = unrestricted access
                0,                    // FALSE
            ) != 0
            {
                sa.lpSecurityDescriptor = sd.as_mut_ptr() as PSECURITY_DESCRIPTOR;
            }
        }
    }

    let mutex_name = windows_sys::w!("Local\\Mycraft_SingleInstance_Mutex");
    let event_name = windows_sys::w!("Local\\Mycraft_SingleInstance_ShowEvent");

    let mutex = unsafe { CreateMutexW(&sa, 0, mutex_name) };
    let err = unsafe { GetLastError() };

    if mutex.is_null() || err == ERROR_ALREADY_EXISTS || err == ERROR_ACCESS_DENIED {
        if !mutex.is_null() {
            unsafe { CloseHandle(mutex) };
        }
        activate_existing_instance();
        return InstanceStatus::AlreadyRunning;
    }

    let event = unsafe { CreateEventW(&sa, 0, 0, event_name) };

    let guard = SingleInstanceGuard { mutex, event };
    if let Ok(mut g) = GUARD.lock() {
        *g = Some(guard);
    }

    InstanceStatus::Primary
}

#[cfg(windows)]
fn activate_existing_instance() {
    unsafe {
        // ASFW_ANY (0xFFFF_FFFF): allow any process to set the foreground window.
        // Since this instance is currently the foreground process (just launched by user),
        // calling AllowSetForegroundWindow grants the running instance permission to focus.
        AllowSetForegroundWindow(0xFFFF_FFFF);

        // Signal the show event so the running instance unhides from tray and restores.
        let event_name = windows_sys::w!("Local\\Mycraft_SingleInstance_ShowEvent");
        let event = OpenEventW(EVENT_MODIFY_STATE, 0, event_name);
        if !event.is_null() {
            SetEvent(event);
            CloseHandle(event);
        }

        // Direct fallback: if the window is already visible or minimized on desktop, restore and focus it.
        let title: Vec<u16> = "Mycraft - Full Stack Dev Box\0".encode_utf16().collect();
        let hwnd = FindWindowW(std::ptr::null(), title.as_ptr());
        if !hwnd.is_null() {
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            BringWindowToTop(hwnd);
            SetForegroundWindow(hwnd);
        }
    }
}

#[cfg(windows)]
pub fn get_show_event() -> Option<isize> {
    GUARD.lock().ok()?.as_ref().map(|g| g.event as isize)
}

pub fn release() {
    #[cfg(windows)]
    if let Ok(mut g) = GUARD.lock() {
        *g = None;
    }
}

#[cfg(not(windows))]
pub fn acquire_or_activate() -> InstanceStatus {
    InstanceStatus::Primary
}

#[cfg(not(windows))]
pub fn get_show_event() -> Option<isize> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn test_single_instance_detection() {
        // Clear any leftover state
        release();

        let status = acquire_or_activate();
        assert!(matches!(status, InstanceStatus::Primary));

        // Attempting to acquire again while Primary is alive should detect AlreadyRunning
        let second = acquire_or_activate();
        assert!(matches!(second, InstanceStatus::AlreadyRunning));

        // After release, acquiring should succeed again as Primary
        release();
        let third = acquire_or_activate();
        assert!(matches!(third, InstanceStatus::Primary));

        release();
    }
}

